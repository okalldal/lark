# lark-rs — Rust Rewrite of the Lark Parsing Toolkit

## Documentation Map

This file is the **agent-facing** operational reference. The companions:

- **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — human-facing tourist map: the
  load→lower→build→parse pipeline and where each module lives. Start here to
  orient.
- **[`GLOSSARY.md`](GLOSSARY.md)** — one-page decoder ring for the parser/lexer
  terms used everywhere.
- **[`docs/decisions/`](docs/decisions/)** — Architecture Decision Records: the
  dated *why* behind load-bearing choices (oracle-first, true-LALR, lookaround
  lowering, …).
- **[`docs/STATUS.md`](docs/STATUS.md)** — the status ledger: what's done, what's
  open, full per-component history.

**Doc-maintenance rule:** a change that alters a load-bearing decision must, in
the same PR, add or supersede an ADR (`docs/decisions/`) and update
`ARCHITECTURE.md` if a module's responsibility moved. Keep the fast-changing
detail in tests, not prose.

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

## Implementation Status (summary)

Phases 1–3 are ✅ complete: LALR + contextual lexer, Earley + SPPF + dynamic lexer,
and full feature parity (common.lark + the bundled stdlib grammars, `%import` file
paths, `%declare`, Indenter/postlex on all parsers, error recovery, CYK, standalone
parser generation). Phase 4 distribution: PyO3 ✅, WASM ✅, C API ✅, benchmarks ✅,
`include_lark!` 🟡 (const-table bake pending). Bank scores: LALR compliance 512/512,
Earley 211/211, dynamic 454/454, CYK 124/124, JSONTestSuite 293/293.

**Per-component tables, open follow-ups, the full lookaround-routing record, and
wild-bank findings: [`docs/STATUS.md`](docs/STATUS.md).**

**Governance / autonomous development.** This repo is developed under a written
constitution — **[`docs/PRINCIPLES.md`](docs/PRINCIPLES.md)** (invariants,
defaults, decision taxonomy, escalation, Definition of Done, merge tiers), with a
decision log in [`docs/decisions/`](docs/decisions/) and the backlog label schema
in [`docs/LABELS.md`](docs/LABELS.md). Cite it when making a non-obvious call;
deviate from a §3 default only with an ADR. Operated by `/roadmap`, `/triage`,
`/next-task`, `/finish-task`, `/review-pr`.

## Testing Philosophy

> "Traditional computers automate what you can specify in code.
>  AI/LLMs automate what you can verify." — Andrej Karpathy

Parsing is hard to implement correctly but easy to verify: **Python Lark is our oracle**.
We generate expected parse trees using Python Lark and compare Rust output against them.

**Rules:**
- Every new grammar feature must have an oracle test before we implement it
- Every bug must be reproducible as a test failure before we fix it
- A suspected performance pathology must be reproducible as a committed, deterministic
  scaling benchmark before we fix it — and the fix targets the cause the profiler names,
  not the one we guessed (see `BENCH.md`)
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
Oracle JSON files are committed so tests run without Python. **Never hand-edit
anything under `tests/fixtures/oracles/` or `tests/standalone/*.rs`** — regenerate
via the tools (`.claude/settings.json` denies direct edits). See `/regen-oracles`.

### Running Tests

```bash
cargo test                          # all tests
cargo test test_arithmetic_oracle   # arithmetic grammar vs oracle
cargo test test_json_oracle         # JSON grammar vs oracle
cargo test test_python_numbers      # Python number literals vs oracle
cargo test test_json_corpus         # 293-file JSONTestSuite (requires submodule)
cargo test test_earley              # Earley oracle + Earley compliance bank
cargo test --test test_wild         # wild-grammar bank (real-world grammars, tests/wild/)

# Deterministic scaling gates — need the work-counter feature. One invocation,
# one build (this is exactly CI's "Scaling gates" step): Earley super-linearity
# (#56), CYK cubic envelope (#87), lexer linear scan (#104), dense-DFA build
# cost (lookaround lowering). Each --test flag also works on its own.
cargo test --features perf-counters --test test_earley_scaling \
  --test test_cyk_scaling --test test_lexer_scaling \
  --test test_lexer_dfa_build_scaling

# L0 whole-lexer differential (the fancy-regex reference backend is TEST-ONLY,
# behind the default-off `fancy-oracle` feature — docs/LOOKAROUND_SCOPE.md).
cargo test -p lark-rs --features fancy-oracle --lib --test test_scanner_differential
```

**Perf regression net (`perf-counters` feature).** Suspected super-linearities are
gated on the *deterministic* work counters in `src/perf.rs` (compiled in only with
`--features perf-counters`; zero overhead otherwise), never wall-clock — see
`BENCH.md`. The four scaling gates above assert flat-per-unit work envelopes
(Earley per-byte, CYK per-n³, lexer per-position, dense-DFA build per terminal);
`examples/profile_parse.rs scaling` prints the same counters as a demo table.

**Earley / ambiguity oracles.** `generate_oracles.py` and
`extract_lark_compliance.py` emit the Earley fixtures in their normal run (no extra
flag). The Earley tests self-gate while a backend is a stub. An `_ambig` node's
children are compared as an *unordered set* (`tree_matches_oracle`) since Lark does
not order them. After an Earley engine change, regenerate the allow-list with
`LARK_COMPLIANCE_WRITE_XFAIL=1 cargo test --test test_earley_compliance` and commit
the shrunk `earley_xfail.json` (see `/xfail-burndown`).

To initialise the JSONTestSuite submodule:
```bash
git submodule update --init tests/corpora/JSONTestSuite
```

### Finishing a Task

Run **`/finish-task`** — review → fast gate → PR → CI callback, codified in
`.claude/commands/finish-task.md`. The always-relevant rules:

- Do **NOT** run the full CI locally before pushing — the `pull_request` run IS
  the full CI (branch pushes alone don't trigger it). One review, one CI run
  per task. `lark-rs/scripts/check.sh` (the full gate) is for reproducing a red
  CI locally, not a routine pre-push step.
- The fast gate is `lark-rs/scripts/check-fast.sh`; the committed pre-push hook
  runs it on every `git push` (the SessionStart hook enables `.githooks`
  automatically).

---

## Architecture

```
src/
  lib.rs              Public API: Lark, LarkOptions, ParserAlgorithm, LexerType
  error.rs            LarkError, GrammarError, ParseError
  tree.rs             Tree, Token (carries type_id: SymbolId), Child. Tree's
                      Drop/Clone are manual worklist impls (#151) — the derived
                      glue recurses to tree depth, which overflows small native
                      stacks (WASM's ~1 MB) on deeply nested parse results
  postlex.rs          Indenter — postlex stream transform (INDENT/DEDENT injection)
  standalone/         Standalone parser generation (#42)
    mod.rs            bake ParseTable + lexer → self-contained Rust source
    runtime.rs        the shared driver (lexer + LALR + tree-shaping), compiled
                      & unit-tested here, include_str!'d into each generated parser
  bin/generate_parser.rs  CLI: `generate-parser --grammar x.lark --output parser.rs`
  grammar/
    mod.rs            load_grammar() entry point; surface Grammar
    loader/           .lark syntax → Grammar, one module per pipeline phase:
      mod.rs            load_grammar()/load_grammar_with_base() + pipeline wiring
      tokenizer.rs      hand-written .lark lexer (Tok enum)
      ast.rs            raw AST (Item, RawRule, RawTerm, Expr, ImportSpec)
      parser.rs         recursive-descent GrammarParser
      compiler.rs       GrammarCompiler state + staging + final Grammar assembly
      ebnf.rs           rule bodies: EBNF expansion, distribution, helper sharing
      terminals.rs      terminal algebra → regex; PatternStr classification
      templates.rs      parameterized template instantiation
      imports.rs        %import resolution (bundled libraries + sibling files)
    intern.rs         SymbolId/SymbolTable + lower(Grammar) → CompiledGrammar
    analysis.rs       NULLABLE / FIRST over SymbolId (no FOLLOW — true LALR(1))
    rule.rs           Rule, RuleOptions (expand1, keep_all_tokens, …)
    symbol.rs         Symbol, Terminal, NonTerminal  (surface grammar only)
    terminal.rs       TerminalDef, Pattern, PatternRe, PatternStr
  lexer/              BasicLexer, ContextualLexer + the combined scanners, one module
                      per concern:
    mod.rs              Lexer trait, LexerConf/LexerBackend, ScannerBackend seam,
                        BasicLexer, ContextualLexer (lazy per-state scanners), LexerState
    plan.rs             ScannerPlan: selection, Python-style ordering, `unless` retyping
    pattern.rs          flag-wrapper algebra (the loader's baked `(?is:…)` + inverse)
    route.rs            THE refusal seam (route_fancy_only_terminal)
    guard.rs            compiled boundary/lookbehind guards + GuardContext
    scanner.rs          the `regex`-crate combined-alternation backend (+ side-probes)
    dfa.rs              the `regex-automata` DFA backend (default; staged build:
                        classify → engines → prefilter), LoweredTerminalMatcher
    dynamic.rs          DynamicMatcher (per-terminal regexes for Earley's dynamic lexer)
    fence.rs            fence-idiom matcher (tag-echo terminals, e.g. heredocs)
    collision.rs        strict-mode regex-collision (#35) + zero-width checks
  parsers/
    mod.rs            ParsingFrontend over a ParserDriver trait — one driver per
                      parser × lexer × postlex wiring (a new configuration is a
                      new impl, not match arms); per-backend builders share the
                      lower + lexer-conf preamble; ParseError construction is
                      centralized in error.rs (unexpected_token's END split)
    lalr.rs           Dense ParseTable, LalrParser, build_lalr_table
    token_source.rs   TokenSource trait + PreLexed / Contextual (lexer⇄parser API)
    tree_builder.rs   TreeBuilder — shared rule→tree shaping (LALR + Earley)
    earley.rs         Earley recognizer + SPPF + forest→tree +
                      dynamic lexer build_chart_dynamic/scan_dynamic
    cyk.rs            CYK parser: CNF conversion (TERM/BIN/UNIT + ε-removal) +
                      O(n³) DP + CNF revert → shared TreeBuilder

tests/
  common/mod.rs       Shared helpers: make_lalr(), load_oracle(), tree_matches_oracle()
  test_oracle.rs      Arithmetic, JSON, Python-number oracle tests
  test_lalr_core.rs   LALR-not-SLR (dangling-else), conflict parity, Earley-errors
  test_compliance.rs  Replays the strip-mined LALR compliance bank (XFAIL/skip-gated)
  test_earley_oracle.rs   Earley + SPPF oracles (resolve + explicit `_ambig`)
  test_earley_dynamic.rs  Curated dynamic-lexer oracles (overlap, %ignore, dynamic_complete)
  test_earley_compliance.rs  Replays the Earley compliance bank (XFAIL-gated)
  test_earley_dynamic_compliance.rs  Replays the dynamic-lexer Earley bank (XFAIL-gated)
  test_earley_stack.rs  #33/#151 net: deep forest walks replayed on a 512 KB
                      thread — crashes if input-depth recursion creeps back into
                      the forest→tree walk or Tree's Drop/Clone
  test_cyk_compliance.rs  Replays the CYK compliance bank (XFAIL-gated)
  test_cyk_scaling.rs Deterministic cubic-envelope gate (#87, perf-counters feature)
  test_recovery.rs    Error-recovery oracle (#43) — single-token-deletion recovery
                      vs Python Lark's `on_error` driver
  test_common.rs      common.lark terminal library vs oracle
  test_indenter.rs    %declare + Indenter/postlex vs oracle (both lexers, all parsers)
  test_lookaround.rs  Lookaround behavioral oracles — engine-agnostic semantics pins
  test_lookaround_scope.rs  Scoreboard for the lookaround routing contract
  test_oracle_coverage.rs  Meta-test: every grammar needs an oracle or quarantine
  test_json_corpus.rs 293-file JSONTestSuite corpus test
  test_wild.rs        Wild-grammar bank replay (tests/wild/, XFAIL-gated) — real-world
                      grammars+inputs vs Python-Lark oracles (digest-compared for big trees)
  test_wild_gap_pins.rs  Distilled pins for each wild-bank root cause fixed
  wild/               Wild-grammar bank: real-world grammars + inputs vendored verbatim
                      from pinned upstream commits (see tests/wild/README.md)
  test_standalone.rs  Standalone parser gen (#42): `include!`s the committed
                      generated parsers + compares to the live oracle; freshness gate
  standalone/         Committed generated parsers (json.rs, arithmetic.rs) — the
                      compile+round-trip fixtures (regenerate: LARK_STANDALONE_WRITE=1)
  grammars/           .lark files used by tests (arithmetic.lark, json.lark, …)
  fixtures/oracles/   Pre-generated oracle JSON (committed, regenerated by tools/)
    lookaround/       cases.json — lookaround lowering gate
    earley/           cases.json + dynamic_cases.json — curated Earley oracles
    compliance/       bank.json + xfail.json + skip.json (LALR);
                      earley_bank.json + earley_xfail.json (Earley basic lexer);
                      earley_dynamic_bank.json + earley_dynamic_xfail.json (dynamic);
                      cyk_bank.json + cyk_xfail.json (CYK)
    wild/             <project>.json + xfail.json — wild-bank oracles (tests/wild/)
  corpora/            Git submodules for external test corpora (JSONTestSuite)
  wasm/               JS smoke tests for the WASM binding (#47); run via `npm test`
                      in lark-rs/wasm/

tools/
  generate_oracles.py        Runs Python Lark, writes fixtures/oracles/**/*.json
  extract_lark_compliance.py Instruments Python Lark's suite → compliance/bank.json
  generate_wild_oracles.py   Replays tests/wild/ through Python Lark → oracles/wild/
                             (needs `pip install regex` for synapse_storm)
```

### Grammar Loading Pipeline (`grammar/loader/`)

One module per phase (`tokenizer` → `parser` → `compiler`, which delegates to
`imports` / `terminals` / `ebnf` / `templates`):

```
.lark text
  → tokenizer::Lexer  (hand-written lexer: Tok enum)
  → parser::GrammarParser  (recursive descent)
      → ast: RawRule / RawTerm / ImportSpec nodes
  → compiler::GrammarCompiler  (lowers AST to Grammar)
      → imports::resolve_import(): parses the bundled src/grammars/common.lark
        through this same loader (cached) and copies the requested terminal(s) —
        no hand-transcribed regex table, so common terminals cannot drift from
        Lark; the other bundled libraries (python/lark/unicode) are likewise
        compiled once per process per option set and cached
      → terminals::resolve_terminals(): sorts alts longest-first, builds TerminalDef
      → ebnf: rule bodies → Symbol sequences; star/plus/opt/group → anonymous
        rules (__anon_*); templates:: instantiates parameterized rules on demand
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

**Terminal regexes are Python-`re` dialect, by decision.** Where the two dialects
assign *different meanings* to the same syntax, lark-rs normalizes toward Python's —
Lark grammars are authored against Python `re`, and oracle fidelity is the project
goal. The load-bearing case: `\<` / `\>` are literal `<` / `>` in Python but
word-boundary assertions in the regex crate (outside a class `\<\>` silently matches
*nothing* where Python matches `"<>"`; inside a class they are a compile error), so
`PatternRe::new` (`normalize_python_escapes`) rewrites exactly those two escapes to
bare chars. Flip side: a grammar author *expecting* the regex crate's word-boundary
`\<`/`\>` is silently overridden — that is the intended trade.

**Lookaround: no runtime backtracking engine — bounded shapes are *lowered* into the
DFA.** The `regex` crate has no lookaround or backreferences; some Lark grammars (the
bundled `python.lark`/`lark.lark`) rely on them. The active plan
([`docs/LEXER_DFA_PLAN.md`](docs/LEXER_DFA_PLAN.md)) builds the combined scanner on a
`regex-automata` DFA and **lowers** supported bounded lookaround into it.
`LexerBackend::Dfa` is the default; **every bundled lookaround terminal lowers**
(`python.STRING` via the M4 opening-guard splice, `lark.REGEXP` and
`python.LONG_STRING` via the Stage-B delimited-token idioms; grammars stay verbatim).
Everything else takes a **categorized build error** (`GrammarError::LookaroundScope`)
under the two-category taxonomy of [`docs/LOOKAROUND_SCOPE.md`](docs/LOOKAROUND_SCOPE.md)
— `OutOfScope` (by-design non-goals: general internal lookahead, variable-width
lookbehind, backrefs) vs `NotYetImplemented` (conservative refusals that double as
promotion tripwires) — through the single refusal seam
`lexer::route_fancy_only_terminal`, enforced identically on every engine path and
scoreboarded by `tests/test_lookaround_scope.rs`. The DFA backend is gated
byte-identical to the fancy-regex reference over the full differential
(`tests/test_scanner_differential.rs`, 0 divergences). **`fancy-regex` is NOT a
runtime dependency** — it survives only as a dev-dependency oracle behind the
default-off TEST-ONLY `fancy-oracle` feature. Per-idiom proofs, the flag-wrapper
strip, the `\G` history, and the superseded lookaround-elimination plan:
[`docs/STATUS.md`](docs/STATUS.md) + [`docs/LEXER_DFA_STATUS.md`](docs/LEXER_DFA_STATUS.md).
One standing exception to "verbatim upstream": `common.lark`'s `ESCAPED_STRING`
keeps its hand-written lookaround-free adaptation (hottest terminal, already linear).

**Interning collapses the rule and terminal namespaces — a release-only hazard.**
`lower()` interns both namespaces into one `by_name` table, so a terminal that
shadows a rule name made the rule resolve to the terminal's id. This is guarded
only by a `debug_assert` in `intern.rs`, so it manifested **in release builds
only** (#144). Anonymous symbols are now disambiguated via a closed `AnonKind`
enum rather than name spelling; keep new interned names namespace-unambiguous.

**Joop-Leo is reimplemented, not ported — and its laziness is load-bearing.**
Python Lark's Leo optimization is dead code (it reads a nonexistent field;
lark-parser/lark#397), so lark-rs's version (`earley.rs`) is an independent
implementation. The lazy spine reconstruction (`load_leo_paths`) is mandatory:
expanding all paths eagerly reintroduces O(n²) (#61) — the forest-size perf
counter is what catches a regression here.

**`dynamic_complete`'s resolve tie-break is a heuristic, not a structural fix.**
The split-point tie-break in `sorted_families` (#90) is keyed on the observation
that the dynamic lexer reverses segmentation order via LIFO completion; it
restores Python's earliest-split-first order empirically. The principled fix
(match Python's group/optional expansion structurally) is a filed follow-up — so
treat this as a known soft spot if dynamic-lexer ambiguity ordering ever drifts.

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

The same script strip-mines three more banks from the other parser test classes:
`earley_bank.json` (TestEarleyBasic), `earley_dynamic_bank.json` (the dynamic-lexer
Earley classes), and `cyk_bank.json` (TestCykBasic), replayed by the matching
`test_*_compliance.rs` harnesses under their own `*_xfail.json` allow-lists.

Enforcement: `tests/test_oracle_coverage.rs` fails the build if a committed grammar has
neither an oracle nor a `QUARANTINE` entry; CI (`.github/workflows/lark-rs.yml`) also
regenerates all three oracle generators and fails if the committed JSON drifts.

---

## Wild-Grammar Bank — Real-World Regression Net + Benchmarks

`tests/wild/` vendors real-world Lark grammars + inputs strip-mined from open
source projects (HCL2/Terraform, MapServer mapfiles, GraphQL SDL, PEP 508,
MistQL, Synapse Storm, Vyper, Quil), each pinned to an upstream commit with its
license and the *exact* Lark options upstream passes — see
[`tests/wild/README.md`](tests/wild/README.md). `tools/generate_wild_oracles.py`
freezes Python Lark's tree per input; `tests/test_wild.rs` replays the bank under
the same XFAIL-burndown discipline as the compliance banks
(`LARK_WILD_WRITE_XFAIL=1` regenerates `oracles/wild/xfail.json`;
`LARK_WILD_TRACE=1` prints per-project timing; `LARK_WILD_DETAILS=1` prints
each failure's build/parse error). `cargo bench --bench wild` runs the same bank
as a recorded performance trend.

Policies: we do not file upstream bugs — a wild grammar bug is either left xfail'd
or patched in the vendored copy and recorded in `meta.json` `local_patches`. A
project may carry an `alt_grammar` workaround, but the bar is strict and
structurally enforced: it must build and be **tree-identical to the original
grammar's Python oracle on every input** (its `*-alt:` failure namespaces are not
xfail-able). Current results and per-project findings: [`docs/STATUS.md`](docs/STATUS.md).

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
- [Lark grammar for Lark](../lark/grammars/lark.lark) — Lark is self-hosting
- [Lark LALR table construction](../lark/parsers/lalr_analysis.py)
- [Lark contextual lexer](../lark/lexer.py) — `ContextualLexer` class
- [Earley + SPPF](../lark/parsers/earley.py) + [earley_forest.py](../lark/parsers/earley_forest.py)
- [Elizabeth Scott's SPPF paper](https://www.sciencedirect.com/science/article/pii/S1571066108001497)
- [JSONTestSuite](https://github.com/nst/JSONTestSuite) — 293-file JSON conformance suite
