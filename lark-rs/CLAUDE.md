# lark-rs — Rust Rewrite of the Lark Parsing Toolkit

## Goal

Rewrite [Lark](https://github.com/lark-parser/lark) in Rust, preserving all its core
differentiators while gaining 10-100× speed and multi-target distribution (PyO3, WASM, C API).

Key differentiators to preserve:
1. **Multi-algorithm**: same EBNF grammar → LALR, Earley, or CYK by changing one flag
2. **Contextual lexer**: parser state narrows which terminals the lexer tries — resolves
   virtually all LALR terminal conflicts without user intervention (Lark's primary USP)
3. **SPPF-based Earley**: handles any CFG, supports explicit ambiguity output
4. **Rich EBNF**: `+`, `*`, `?`, `|`, char ranges, parameterized templates, priorities,
   aliases, `%import` grammar composition
5. **Automatic tree building**: `Tree` / `Token` without user action code
6. **`?rule` (expand1)**, `_rule` (transparent), `!rule` (keep all tokens) modifiers

## Testing Philosophy

> "Traditional computers automate what you can specify in code.
>  AI/LLMs automate what you can verify." — Andrej Karpathy

Parsing is hard to implement correctly but easy to verify: **Python Lark is our oracle**.
We generate expected parse trees using Python Lark and compare Rust output against them.

**Rules:**
- Every new grammar feature must have an oracle test before we implement it
- Every bug must be reproducible as a test failure before we fix it
- Prefer end-to-end tests over unit tests — the oracle checks the full pipeline
- Corpus tests (JSONTestSuite) are kept at 100% oracle agreement; never regress them
- Never write an oracle test that depends on an arbitrary lexer tie-break — two
  terminals matching the same span at equal priority, which Lark resolves by an
  incidental regex-source-length sort that lark-rs does not reproduce. Disambiguate
  the grammar with explicit terminal priority instead, exactly as the Lark authors
  do (e.g. `NON_SEPARATOR_STRING.2` in `csv.lark`). Both engines honor priority
  first, so the result is principled. (Measured 2026-06-02: 0 of 140 compliance-bank
  divergences are tie-breaks — a discipline for our grammars, not a gap to chase.)

### Generating Oracles

```bash
cd lark-rs
python3 tools/generate_oracles.py          # regenerates all fixtures/oracles/**/*.json
```

The script uses Python Lark (`pip install lark`) to produce ground-truth parse trees.
Oracle JSON files are committed so tests run without Python.

### Running Tests

```bash
cargo test                          # all tests (~0.2 s)
cargo test test_arithmetic_oracle   # arithmetic grammar vs oracle
cargo test test_json_oracle         # JSON grammar vs oracle
cargo test test_python_numbers      # Python number literals vs oracle
cargo test test_json_corpus         # 293-file JSONTestSuite (requires submodule)
```

To initialise the JSONTestSuite submodule:
```bash
git submodule update --init tests/corpora/JSONTestSuite
```

### Before Pushing — Local CI Gate

`lark-rs/scripts/check.sh` runs **exactly** what GitHub Actions runs (the `Format`
pre-commit job, `cargo test --all`, and the oracle-freshness gate). Run it before
pushing so a red CI is caught locally first:

```bash
lark-rs/scripts/check.sh
```

Enable the committed pre-push hook once per clone so it runs automatically on
every `git push` (and blocks the push if any gate fails):

```bash
git config core.hooksPath .githooks
```

Requirements: `pip install lark pre-commit` and the JSONTestSuite submodule
(above). **Never push without a green gate.**

---

## Architecture

```
src/
  lib.rs              Public API: Lark, LarkOptions, ParserAlgorithm, LexerType
  error.rs            LarkError, GrammarError, ParseError
  tree.rs             Tree, Token (carries type_id: SymbolId), Child
  grammar/
    mod.rs            load_grammar() entry point; surface Grammar
    loader.rs         .lark syntax lexer + parser + compiler (EBNF → Grammar)
    intern.rs         SymbolId/SymbolTable + lower(Grammar) → CompiledGrammar
    analysis.rs       NULLABLE / FIRST over SymbolId (no FOLLOW — true LALR(1))
    rule.rs           Rule, RuleOptions (expand1, keep_all_tokens, …)
    symbol.rs         Symbol, Terminal, NonTerminal  (surface grammar only)
    terminal.rs       TerminalDef, Pattern, PatternRe, PatternStr
  lexer.rs            Scanner (id-based), BasicLexer, ContextualLexer, LexerState
  parsers/
    mod.rs            ParsingFrontend — lowers grammar, wires lexer + parser
    lalr.rs           Dense ParseTable, LalrParser, build_lalr_table
    earley.rs         Earley + SPPF (Phase 2 — stub)

tests/
  common/mod.rs       Shared helpers: make_lalr(), load_oracle(), tree_matches_oracle()
  test_oracle.rs      Arithmetic, JSON, Python-number oracle tests
  test_lalr_core.rs   LALR-not-SLR (dangling-else), conflict parity, Earley-errors
  test_compliance.rs  Replays the strip-mined compliance bank (XFAIL/skip-gated)
  test_oracle_coverage.rs  Meta-test: every grammar needs an oracle or quarantine
  test_json_corpus.rs 293-file JSONTestSuite corpus test
  grammars/           .lark files used by tests (arithmetic.lark, json.lark, …)
  fixtures/oracles/   Pre-generated oracle JSON (committed, regenerated by tools/)
    compliance/       bank.json (strip-mined), xfail.json, skip.json
  corpora/            Git submodules for external test corpora (JSONTestSuite)

tools/
  generate_oracles.py        Runs Python Lark, writes fixtures/oracles/**/*.json
  extract_lark_compliance.py Instruments Python Lark's suite → compliance/bank.json
```

### Grammar Loading Pipeline (`loader.rs`)

```
.lark text
  → GrammarLexer      (hand-written lexer: Tok enum)
  → GrammarParser     (recursive descent)
      → RawRule / RawTerm / ImportSpec AST nodes
  → GrammarCompiler   (lowers AST to Grammar)
      → EBNF expansion: star/plus/opt/group → anonymous rules (__anon_*)
      → resolve_import(): reads common.lark stubs, registers terminals
      → compile_term(): sorts alts longest-first, builds TerminalDef
      → compile_rule_body(): lowers rule bodies to Symbol sequences
  → Grammar { rules, terminals, ignore, start }   (surface, string-named)
```

### Interning Pipeline (`intern.rs`)

The surface `Grammar` is **lowered** to a `CompiledGrammar` before the engine
touches it. Lowering interns every symbol to a `Copy` `SymbolId`, assigning all
terminal ids first (`$END` = id 0) so terminals occupy `[0, n_terminals)` and
non-terminals `[n_terminals, len)`. It also synthesizes the augmented start rules
(`$root_X → X`) and precomputes every tree-shaping flag, so the engine never
inspects a symbol name again.

```
Grammar (string-named, name-prefix semantics)
  → lower()
      → SymbolTable    intern terminals (id 0 = $END), then non-terminals
      → CompiledRule   { origin, expansion: Vec<SymbolId>, options,
                         tree_name, transparent, is_start }   ← flags, not prefixes
  → CompiledGrammar { symbols, rules, terminals, ignore, start }
```

The flags replace the old name-prefix sniffing entirely:
`is_start` (was `name.starts_with("$root_")`), `transparent` (was a leading `_` /
`__anon_` check), `filter_out` (per-terminal-id bool), and terminal-vs-non-terminal
(was a name set + `$` check) is now just `id < n_terminals`.

### LALR Construction Pipeline (`lalr.rs`)

```
CompiledGrammar
  → GrammarAnalysis   (NULLABLE / FIRST over SymbolId; no FOLLOW)
  → LR0Builder        (closure + goto → item sets / transitions, keyed by SymbolId)
  → LookaheadComputer (true LALR(1) lookaheads: spontaneous generation + propagation)
  → build_lalr_table  dense tables, conflict detection by rule priority
  → ParseTable        { action: Vec<Vec<Option<Action>>>  [state][terminal_id],
                        goto:   Vec<Vec<Option<u32>>>      [state][nonterminal_index] }
```

Both tables are dense and indexed directly by id — the parse loop is an array
index per token, never a string hash. Transparent rules splice via a
`StackValue::Inline` rather than a post-hoc tree-name scan, and ACCEPT is the
`is_start` flag — no name inspection anywhere on the engine path.

### Parse-Tree Assembly

After each REDUCE, `apply_rule_options()` post-processes children:
1. Filter punctuation tokens (unnamed `__` / `_` terminals) unless `keep_all_tokens`
2. Flatten anonymous EBNF helper nodes (`__anon_*`) into parent's child list
3. `expand1` (`?rule`): if exactly one child and no alias, return that child as-is
   — returns `Child` (Token or Tree), not always a Tree

4. Inline transparent rules: a `_name` rule (single leading underscore) or
   `__anon_*` EBNF helper is spliced into the parent's child list, not kept as a
   wrapper node.

---

## Implementation Status

### 🔧 Phase 1 — LALR + Contextual Lexer (Core working, bugs to fix)

The parser handles JSON, arithmetic, and Python number literals correctly against the
oracle. Three correctness bugs need fixing before Phase 2 starts (see Known Bugs).

| Component | Status | Notes |
|-----------|--------|-------|
| Grammar lexer | ✅ | Handles all EBNF operators, priorities, aliases |
| Grammar parser | ✅ | Recursive descent, multi-line alternation |
| EBNF expansion | ✅ | `*`, `+`, `?`, groups → anonymous rules |
| `%import` + alias | ✅ | `%import common.X -> Y` registers under alias |
| `%ignore` | ✅ | Inline regex or terminal name |
| `%declare` | ✅ (parse only) | No semantic action yet |
| Parameterised templates | ✅ | `_sep{x, sep}: x (sep x)*` |
| FIRST/FOLLOW/NULLABLE | ✅ | Standard fixed-point algorithm |
| LR(0) item sets | ✅ | Canonical collection |
| LALR(1) lookaheads | ✅ | True LALR(1) via spontaneous-generation + propagation (`LookaheadComputer`) |
| Conflict detection | ✅ | S/R → shift; R/R → priority, else `GrammarError::Conflict`; matches Lark outcomes |
| ParseTable (ACTION/GOTO) | ✅ | Shift/Reduce/Accept |
| BasicLexer | ✅ | Single combined regex (leftmost-first, like Python `re`) + `unless` keyword retyping |
| ContextualLexer | ✅ | Per-state `Scanner`; per-state `unless` retyping; always_accept for ignores |
| Terminal priority ordering | ✅ | (-priority, -pattern_len, name) |
| Within-terminal alt ordering | ✅ | Longest-first (mirrors Python Lark) |
| Tree assembly | ✅ | `expand1`, anon inlining |
| Transparent `_rule` inlining | ✅ | `is_anonymous_rule` flattens `__anon_*` and `_name` rules; alias exempt |
| `keep_all_tokens` | ✅ | |
| Aliases (`-> name`) | ✅ | Correctly overrides `expand1` |
| Token positions (line/col) | ✅ | Char-based columns; end_line/end_column newline-aware |
| Oracle test harness | ✅ | arithmetic, JSON, python_numbers, lalr_core |
| JSONTestSuite corpus | ✅ | 293/293 oracle agreement |
| Compliance bank | ✅ | 257 grammars strip-mined from Python Lark's suite; 338/508 ≈ 66% agree (XFAIL-gated) |
| Oracle-coverage enforcement | ✅ | Meta-test + CI freshness gate |

### ⬜ Phase 2 — Earley + SPPF

**Phase 2 stays frozen** until the compliance-bank parity climbs further (it is
currently 350/512 ≈ 68%). All Phase-1 correctness bugs (BUG-1 through BUG-7) are now
fixed: true LALR(1) lookaheads, fail-loud conflicts, the keyword lexer (BUG-3),
transparent `_rule` inlining (BUG-4), char-based positions (BUG-5), the Earley
fail-loud guard (BUG-6), and recursive templates (BUG-7). The core now fails loudly
instead of silently mis-resolving.

Earley is the second USP. It handles grammars LALR cannot (ambiguous, non-deterministic).
Requesting `ParserAlgorithm::Earley` now returns an explicit error (was a silent
LALR fallback).

| Component | Status | Notes |
|-----------|--------|-------|
| Earley recogniser | ⬜ | Aycock/Earley algorithm |
| SPPF forest construction | ⬜ | Shared Packed Parse Forest |
| Forest → tree conversion | ⬜ | `ambiguity='resolve'` picks one |
| `ambiguity='explicit'` | ⬜ | Returns multiple trees |
| Dynamic lexer | ⬜ | Tokenise lazily with parser context |
| `dynamic_complete` | ⬜ | Try all tokenisations |

### ⬜ Phase 3 — Full Feature Parity

| Component | Status | Notes |
|-----------|--------|-------|
| Complete `common.lark` stubs | ⬜ | ~40 common terminals |
| `%import` from file path | ⬜ | Relative imports |
| Grammar standard library | ⬜ | SQL, Python, … |
| Indenter / postlex | ⬜ | Python-style INDENT/DEDENT |
| Standalone parser gen | ⬜ | Emit self-contained Rust or Python |
| Error recovery | ⬜ | Insert/delete tokens on failure |
| CYK parser | ⬜ | Highly ambiguous grammars |

### ⬜ Phase 4 — Distribution

| Component | Status | Notes |
|-----------|--------|-------|
| PyO3 Python binding | ⬜ | Drop-in speedup for Python Lark users |
| WASM target | ⬜ | Browser/Node.js |
| C API | ⬜ | `lark_h` crate |
| `include_lark!` proc-macro | ⬜ | Compile-time grammar validation |
| Benchmarks vs Python Lark | ⬜ | JSON / Python / SQL |

---

## Known Bugs — Must Fix Before Phase 2

These are Phase-1 correctness issues discovered during code review (2026-06-02).
They pass current tests only because the test grammars (JSON, arithmetic) don't
exercise the failure modes. Each needs an oracle test that fails, then a fix.

### BUG-1 ✅ FIXED — Parser is now true LALR(1)

**File:** `src/parsers/lalr.rs`

`LookaheadComputer` was rewritten as a canonical LALR(1) lookahead computation
(spontaneous-generation + propagation with a real LR(1) closure that handles
ε-rules) and wired into `build_lalr_table`, replacing the SLR FOLLOW-set lookup.
The previous dead code also baked FOLLOW sets into propagation, so it would not
have been true LALR even if called.

**Oracle:** `lalr_core/dangling_else` — a grammar that is LALR(1) but not SLR(1)
builds cleanly and parses identically to Python Lark.

### BUG-2 ✅ FIXED — Conflict detection + rule-priority resolution

**File:** `src/parsers/lalr.rs`, `src/error.rs`

Conflicts are collected during table construction and resolved exactly as Python
Lark does: S/R → shift (no error); R/R → highest `RuleOptions.priority`, and a tie
raises the new `GrammarError::Conflict`. R/R is no longer silent last-writer-wins.

**Oracle (outcome parity):** `lalr_core/conflicts` — for each grammar, lark-rs
errors iff Python Lark raises `GrammarError` at construction.

### BUG-3 ✅ FIXED — Keyword/identifier disambiguation via Lark's `unless`

**File:** `src/lexer.rs`

The earlier diagnosis ("Python guarantees longest-match; match each terminal and
pick the longest span") was inaccurate. Python Lark's lexer is **not** true
longest-match: it sorts terminals `(-priority, -max_width, -len(value), name)` and
takes the first alternation match (leftmost-first, identical to the `regex`
crate's semantics), **plus** an `unless` callback — a string terminal whose value
is fully matched by a same-priority regex terminal (e.g. the keyword `if` inside
`CNAME`) is dropped from the alternation and the regex match is retyped back to the
keyword when the matched text equals it. That is what makes `if` lex as `IF` while
`iffy` stays `NAME`, with no cross-terminal length scan.

The fix unifies `BasicLexer`/`ContextualLexer` onto one `Scanner` that implements
`unless` (computed per state for the contextual lexer, exactly as Python builds one
`TraditionalLexer` per parser state), and drops the obsolete `MAX_GROUPS = 98`
chunking (Rust's `regex` crate has no named-group limit).

**Oracle:** `keywords/cases` — `keywords.lark` (un-quarantined) parses `iffy`,
`elsewhere`, `whiled` as `NAME` and `if`/`while` as keywords, matching Python Lark.

### BUG-4 ✅ FIXED — Transparent `_rule` trees now inlined

**File:** `src/parsers/lalr.rs`

`is_anonymous_rule` now flattens any node whose name starts with `_` — covering
both `__anon_*` EBNF helpers and `_name` transparent rules — so a `_name` rule's
children splice into the parent instead of leaking as a `Tree("_name", …)`.
Aliased rules are exempt: the node carries the alias name (which has no leading
underscore), so an alias overrides transparency, matching Python Lark.

**Oracle:** `csv/cases` — `csv.lark` (un-quarantined); `_anything` inlines its token
into `row`. `_anything`'s alternatives overlap on bare letter runs
(`WORD`/`NON_SEPARATOR_STRING`), so `csv.lark` gives `NON_SEPARATOR_STRING` an
explicit `.2` priority so the choice is deterministic (see the tie-break rule under
Testing Philosophy).

### BUG-5 ✅ FIXED — Token positions are char-based and newline-aware

**File:** `src/lexer.rs` (ContextualLexer::next_token)

`next_token` now walks `value.chars()` to compute `end_line`/`end_column`, so columns
count characters (not bytes — correct for non-ASCII) and a token spanning a newline
advances the line and resets the column, mirroring `LexerState::advance_by`.

**Oracle:** `tests/test_positions.rs` — expectations taken from Python Lark (a
multi-line `BLOCK` ending at line 2 col 4; `café` ending at col 5), since the tree
oracles do not capture positions.

### BUG-6 ✅ FIXED — Earley errors instead of silently falling back

**File:** `src/parsers/mod.rs`, `src/lib.rs`

`ParserAlgorithm::Earley` now returns an explicit "not yet implemented" error
(matching CYK), and `LarkOptions::default()` uses `Lalr`. Guarded by
`test_lalr_core::test_earley_errors_instead_of_silent_fallback`.

### BUG-7 ✅ FIXED — Recursive templates memoized; `~N` expanded iteratively

**File:** `src/grammar/loader.rs`

A self-recursive template (`_sep{x,d}: x | _sep{x,d} d x`) used to recurse
infinitely during instantiation and abort the process. Two root causes, both now
fixed to match Python Lark (which builds and parses this grammar):

1. **Substitution skipped nested template-usage args** — `subst_expr` cloned a
   `TemplateUsage` verbatim, so the inner `_sep{item, delim}` never became
   `_sep{NUMBER, ","}`. Added `subst_value`, which recurses into a usage's args.
2. **No instantiation memo** — even a correct self-reference recursed forever.
   `instantiate_template` now memoizes by a canonical `name<args>` key and registers
   the instance *before* compiling its body, so the self-reference resolves to the
   rule already being built (a normal recursive rule).

Beyond un-skipping the recursive-template grammar, fix (1) corrected nested template
substitution generally — 8 compliance-bank XFAIL entries flipped to passing.

The other historical aborter, `"A"~8191`, is already safe: the exact-repetition
case (`n == m`) inlines the copies into one heap-allocated rule and LR(0) construction
is iterative, so it no longer blows the stack. `skip.json` is now empty.

---

## Key Design Decisions & Gotchas

**Terminal ordering matters.** Terminals are sorted `(-priority, -pattern_len, name)` before
the combined regex is built. Higher priority and longer patterns come first so that, e.g.,
`OCT` (`0[oO][0-7]…`) beats `INT` (`[0-9]…`) at `"0o777"`. Get this wrong and the lexer
silently picks the wrong terminal.

**Within-terminal alternatives are sorted longest-first.** Python Lark does this internally.
A terminal like `FLOAT` with 4 alternatives must list `decimal+exponent` before `decimal`
so that `"3.14e10"` matches the right alternative.

**`expand1` returns `Child`, not `Tree`.** The `?rule` modifier must be able to return a
bare `Token` when the rule has a single terminal child — e.g., `?atom: NAME` should yield
the `NAME` token directly, not `Tree("atom", [Token])`. This propagates all the way up
(`?factor → ?term → ?expr`). The stack stores `StackValue::Token | StackValue::Tree`
for this reason.

**SHIFT vs GOTO uses the real terminal name set.** A naive heuristic (`!name.starts_with('_')`)
misclassifies anonymous non-terminals like `__anon_opt_0`. Always look up against the actual
`grammar.terminals` name set: if it's in that set (or starts with `__ANON_`) it's a terminal
(ACTION); otherwise it's a non-terminal (GOTO).

**Ignore terminals must be in `always_accept`.** The contextual lexer only tries terminals
listed for the current parser state. `%ignore` terminals appear in NO state's lookahead set,
so they must be passed as `always_accept` when building `ContextualLexer`. The parse loop
then explicitly skips tokens whose `type_` is in `lexer.ignore()`.

**Import aliases are the registered name.** `%import common.WS -> _WS` means the terminal
is `_WS` in this grammar, not `WS`. Store and look up by alias.

**`LookaheadComputer` computes true LALR(1) lookaheads** (spontaneous-generation +
propagation). Conflict detection depends on its precision: SLR FOLLOW sets would
over-report conflicts, so accurate `GrammarError::Conflict` reporting requires it.

**`regex` crate has no lookahead or backreferences.** Some Python Lark grammars in the
wild rely on these. Document as a known parity gap when adding Phase-3 grammar library.

---

## Recommended Work Order (Next Sessions)

All Phase-1 correctness bugs (BUG-1 through BUG-7) are **done**. The compliance bank
is the regression net: fixing a bug flips XFAIL entries to passing — regenerate
`xfail.json` and watch parity rise (BUG-3 flipped 3, BUG-7 flipped 8, lifting the bank
to ~68%). Next: keep widening compliance parity (the remaining XFAILs are unimplemented
features and structural bugs — measure with the divergence breakdown), then start
Phase 2 — Earley + SPPF.

### Core IR consolidation (done 2026-06-03)

The engine's spine was migrated off the stringly-typed surface grammar onto an
interned IR (`intern.rs`): `Copy` `SymbolId`s, typed flags instead of name-prefix
semantics, and dense array-indexed ACTION/GOTO tables (see the Interning + LALR
pipelines above). This was the behavior-preserving foundation step — the full
oracle suite, JSON corpus, and compliance bank stayed green throughout. **Build
Phase 2 (Earley/SPPF) on `CompiledGrammar`**, keying forest nodes by `SymbolId`,
not names. Deferred until a profiler justifies them: FIRST/FOLLOW bitsets, the
DeRemer–Pennello relational lookahead method (the current `lr1_closure` snapshots
its map each fixpoint iteration — correct but quadratic on large grammars), and
zero-copy token spans.

### The compliance bank — your regression net

`tools/extract_lark_compliance.py` instruments Python Lark and runs its LALR test
classes, capturing every `(grammar, options, input, tree|error)` into
`tests/fixtures/oracles/compliance/bank.json` (257 grammars). `tests/test_compliance.rs`
replays it, gated by `xfail.json` (known failures) and `skip.json` (process-aborting
grammars). The build fails only on **regressions**. After a fix:
`LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_compliance` regenerates the
allow-list; commit the shrunk `xfail.json`. `LARK_COMPLIANCE_TRACE=1` prints each
grammar before it runs (use it to find a new process-aborting grammar).

Enforcement: `tests/test_oracle_coverage.rs` fails the build if a committed grammar has
neither an oracle nor a `QUARANTINE` entry; CI (`.github/workflows/lark-rs.yml`) also
regenerates both oracle generators and fails if the committed JSON drifts.

---

## Adding New Grammar Features — Checklist

1. Add a test case to `generate_oracles.py` and regenerate
2. Confirm Python Lark produces the expected tree (oracle)
3. Run `cargo test` — watch it fail
4. Implement the feature
5. Run `cargo test` — watch it pass
6. Commit both the oracle JSON and the implementation together

---

## External Resources

- [Lark Python source](https://github.com/lark-parser/lark) — the reference implementation
- [Lark grammar for Lark](lark/grammars/lark.lark) — Lark is self-hosting
- [Lark LALR table construction](lark/parsers/lalr_analysis.py)
- [Lark contextual lexer](lark/lexer.py) — `ContextualLexer` class
- [Earley + SPPF](lark/parsers/earley.py) + [earley_forest.py](lark/parsers/earley_forest.py)
- [Elizabeth Scott's SPPF paper](https://www.sciencedirect.com/science/article/pii/S1571066108001497)
- [JSONTestSuite](https://github.com/nst/JSONTestSuite) — 293-file JSON conformance suite
