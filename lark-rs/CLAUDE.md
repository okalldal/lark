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
cargo test test_earley              # Earley oracle + Earley compliance bank (Phase 2)
```

**Earley / ambiguity oracles (Phase 2).** `generate_oracles.py` and
`extract_lark_compliance.py` already emit the Earley fixtures as part of their
normal run (no extra flag). The Earley tests **self-gate**: while the backend is a
stub they skip via `common::earley_unimplemented()`, then start enforcing the
moment Earley builds. An `_ambig` node's children are compared as an *unordered
set* (`tree_matches_oracle` handles this) since Lark does not order them. After an
Earley engine change, regenerate the allow-list with
`LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_earley_compliance` and commit
the shrunk `earley_xfail.json` — the same XFAIL-burndown loop the LALR bank used.

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
  lexer.rs            Scanner (id-based), BasicLexer, ContextualLexer, LexerState,
                      DynamicMatcher (per-terminal regexes for Earley's dynamic lexer)
  parsers/
    mod.rs            ParsingFrontend — lowers grammar, wires lexer + parser
    lalr.rs           Dense ParseTable, LalrParser, build_lalr_table
    token_source.rs   TokenSource trait + PreLexed / Contextual (lexer⇄parser API)
    tree_builder.rs   TreeBuilder — shared rule→tree shaping (LALR + Earley)
    earley.rs         Earley recognizer + SPPF + forest→tree (Sprints 1–2) +
                      dynamic lexer build_chart_dynamic/scan_dynamic (Sprint 5)

tests/
  common/mod.rs       Shared helpers: make_lalr(), load_oracle(), tree_matches_oracle()
  test_oracle.rs      Arithmetic, JSON, Python-number oracle tests
  test_lalr_core.rs   LALR-not-SLR (dangling-else), conflict parity, Earley-errors
  test_compliance.rs  Replays the strip-mined LALR compliance bank (XFAIL/skip-gated)
  test_earley_oracle.rs   Earley + SPPF oracles (resolve + explicit `_ambig`); self-gates until the engine lands
  test_earley_dynamic.rs  Curated dynamic-lexer oracles (overlap, %ignore, dynamic_complete)
  test_earley_compliance.rs  Replays the Earley compliance bank (XFAIL-gated); the Phase-2 regression net
  test_earley_dynamic_compliance.rs  Replays the dynamic-lexer Earley bank (XFAIL-gated)
  test_common.rs      common.lark terminal library vs oracle (Phase 3) — each
                      user-facing common terminal lexes as Python Lark's does
  test_oracle_coverage.rs  Meta-test: every grammar needs an oracle or quarantine
  test_json_corpus.rs 293-file JSONTestSuite corpus test
  grammars/           .lark files used by tests (arithmetic.lark, json.lark, …)
  fixtures/oracles/   Pre-generated oracle JSON (committed, regenerated by tools/)
    earley/           cases.json — curated Earley oracles (resolve + explicit);
                      dynamic_cases.json — curated dynamic-lexer oracles (Sprint 5)
    compliance/       bank.json + xfail.json + skip.json (LALR);
                      earley_bank.json + earley_xfail.json (Earley basic lexer);
                      earley_dynamic_bank.json + earley_dynamic_xfail.json (dynamic lexer)
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
      → resolve_import(): parses the bundled src/grammars/common.lark through this
        same loader (cached) and copies the requested terminal(s) — no
        hand-transcribed regex table, so common terminals cannot drift from Lark
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
`__anon_` check), and terminal-vs-non-terminal (was a name set + `$` check) is now
just `id < n_terminals`. Token filtering is **per rule position**, not per terminal:
each `CompiledRule` carries a `filter_pos: Vec<bool>` parallel to its expansion
(lowered from each `Symbol::Terminal` occurrence's own `filter_out`), so a terminal
that is unified for lexing can still be kept at one rule position and dropped at
another — Lark's model (per-position token filtering, see `docs/archive/COMPLIANCE_PARITY.md` §M6).

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

### ✅ Phase 1 — LALR + Contextual Lexer

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
| Compliance bank | ✅ | 257 grammars strip-mined from Python Lark's suite; 510/512 ≈ 99.6% agree (XFAIL-gated) |
| `strict` mode | ✅ | `strict=True` raises on shift/reduce conflicts (reduce/reduce already fatal), like Lark |
| `g_regex_flags` | ✅ | Global regex flags (e.g. `IGNORECASE`) applied to every terminal via a combined-regex prefix |
| Oracle-coverage enforcement | ✅ | Meta-test + CI freshness gate |

### ✅ Phase 2 — Earley + SPPF

All six sprints complete. LALR compliance 510/512 ≈ 99.6%; Earley basic bank
211/211 (clean); dynamic-lexer bank 446/454 ≈ 98.2%. Open items tracked as GitHub
issues: #32 (XFAIL burndown — cluster 1, "nested `_ambig` through a transparent
`_rule`/EBNF helper", fixed by porting Lark's `AmbiguousExpander`; the
`%ignore`-of-content and `dynamic_complete` tie-break clusters remain), #31 (perf
benchmark), #33 (de-recurse forest walk), #35 (strict regex-collision, deferred —
needs FSM engine).

| Component | Status | Notes |
|-----------|--------|-------|
| Ambiguity test harness (Sprint 0) | ✅ | Earley oracles + `_ambig` set-matcher + Earley compliance bank (147 grammars), self-activating the moment the frontend builds |
| Earley recogniser | ✅ | Sprint 1: predict/scan/complete over `SymbolId`. Now reimplemented on top of the Sprint-2 chart (`recognize` = "did the start node build?"), so it accepts exactly what `parse` parses. Verified by `test_earley_recognizer` |
| SPPF forest construction | ✅ | Sprint 2: Elizabeth Scott's binarized SPPF (symbol / intermediate / packed nodes, arena-allocated by `NodeId`, held-completion nullable handling). Joop-Leo transitives omitted (dead code in the reference) |
| Forest → tree conversion | ✅ | Sprint 2: `Transformer` walks the SPPF and reuses `TreeBuilder::assemble`; `ambiguity='resolve'` picks the highest-priority derivation (Lark's `ForestSumVisitor` order). Verified ≡ LALR on every unambiguous oracle by `test_earley_parity` |
| `ambiguity='explicit'` | ✅ | Sprint 2: emits `_ambig` forests; curated cases pass, bank 211/211 (clean — the `AmbiguousExpander` port lifts an ambiguous transparent `_rule`/EBNF-helper child's ambiguity up into the parent) |
| Dynamic lexer | ✅ | Sprint 5: scanning folded into the Earley loop (`xearley.py` port) — terminals tried at each position are exactly those the parser predicts. `LexerType::Dynamic`. Delayed-match buffer for variable-length tokens + `%ignore` carry-over. Terminal priorities feed the forest sum (the basic lexer consumes them in its ordering; the dynamic lexer does not). Bank 446/454 ≈ 98.2% |
| `dynamic_complete` | ✅ | Sprint 5: `LexerType::DynamicComplete` — also explores every shorter tokenization, so all segmentations are considered |

### ⬜ Phase 3 — Full Feature Parity

| Component | Status | Notes |
|-----------|--------|-------|
| Complete `common.lark` stubs | ✅ | The full upstream `common.lark` is bundled (`src/grammars/common.lark`) and parsed through lark-rs's own terminal-algebra loader, not a hand-transcribed regex table — so common terminals can't drift. Added `CR`/`LF`/`SQL_COMMENT` + the `_EXP`/`_STRING_*` helpers; one documented lookbehind adaptation for `ESCAPED_STRING`. Pinned by `test_common.rs` |
| `%import` from file path | ✅ | Relative imports (`%import .module (X, ...)`) resolve against the importing grammar's directory (`LarkOptions.base_path`), load through `load_grammar`, and copy the requested terminal/rule — a rule pulls in its dependency closure, mangled under the module name (Python's `_get_mangle`). Pinned by `test_imports.rs` (oracles in `fixtures/oracles/imports/`, grammars under `tests/grammars/imports/`) |
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

## Open Work

All open tasks are tracked as GitHub issues. Current priority order for Phase 3:
#39 (`%import` file paths) → #45 (`%declare`) → #41 (Indenter/postlex) → #32
(Earley XFAIL burndown) → #31 (Earley perf benchmark) → #40 (grammar stdlib) →
#43 (error recovery) → #42 (standalone parser) → #44 (CYK).

Deferred until specialist work is available: #35 (strict regex-collision, needs FSM
engine), #33 (de-recurse forest walk, profiler-gated).

Phase 4 distribution (#46–#50) follows after Phase 3 is substantially complete.

---

## Compliance Bank — Regression Net

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
