# lark-rs — Implementation Status, Open Work & Wild-Bank Findings

The detailed status ledger, moved out of [`CLAUDE.md`](../CLAUDE.md) (which keeps
the short summary + instructions). This file is the record of *what* is done and
*how* it landed; consult it when you need the history or per-component detail.

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
| `%declare` | ✅ | Registers a pattern-less terminal (excluded from every scanner, still interned) so rules/postlex can reference it; see Phase 3 |
| Parameterised templates | ✅ | `_sep{x, sep}: x (sep x)*` |
| FIRST/FOLLOW/NULLABLE | ✅ | Standard fixed-point algorithm |
| LR(0) item sets | ✅ | Canonical collection |
| LALR(1) lookaheads | ✅ | True LALR(1) via spontaneous-generation + propagation (`LookaheadComputer`) |
| Conflict detection | ✅ | S/R → shift; R/R → priority, else `GrammarError::Conflict`; matches Lark outcomes |
| ParseTable (ACTION/GOTO) | ✅ | Shift/Reduce/Accept |
| BasicLexer | ✅ | Single combined regex (leftmost-first, like Python `re`) + `unless` keyword retyping |
| ContextualLexer | ✅ | Per-state `Scanner`; per-state `unless` retyping; always_accept for ignores. States sharing a terminal set share one scanner (Python's `lexer_by_tokens` dedup, 4–5× on the wild bank) and scanners build lazily on first use; an eager full-terminal validation build (Python's `root_lexer` analog) keeps scope errors at construction time |
| Terminal priority ordering | ✅ | (-priority, -pattern_len, name) |
| Within-terminal alt ordering | ✅ | Longest-first (mirrors Python Lark) |
| Tree assembly | ✅ | `expand1`, anon inlining |
| Transparent `_rule` inlining | ✅ | `is_anonymous_rule` flattens `__anon_*` and `_name` rules; alias exempt |
| `keep_all_tokens` | ✅ | |
| Aliases (`-> name`) | ✅ | Correctly overrides `expand1` |
| Token positions (line/col) | ✅ | Char-based columns; end_line/end_column newline-aware |
| Oracle test harness | ✅ | arithmetic, JSON, python_numbers, lalr_core |
| JSONTestSuite corpus | ✅ | 293/293 oracle agreement |
| Compliance bank | ✅ | 257 grammars strip-mined from Python Lark's suite; 512/512 = 100% agree (XFAIL-gated) |
| `strict` mode | ✅ | `strict=True` raises on shift/reduce conflicts (reduce/reduce already fatal) **and** on same-priority regex-terminal collisions (#35), like Lark |
| Strict regex-collision (#35) | ✅ | `strict=True` rejects two same-priority *regex* terminals whose languages overlap, mirroring Python's interegular check. lark-rs has no FSM in `regex`, so each terminal is compiled to a whole-match DFA (`regex-automata`) and a **product-construction** BFS decides intersection-emptiness, reporting the shortest witness string. Excludes string-literal terminals (Python's `PatternStr`) via a `TerminalDef::string_type` flag so a keyword like `IF: "if"` is never flagged against `/[a-z]+/`. `src/lexer/collision.rs::check_regex_collisions` |
| `g_regex_flags` | ✅ | Global regex flags (e.g. `IGNORECASE`) applied to every terminal via a combined-regex prefix |
| Oracle-coverage enforcement | ✅ | Meta-test + CI freshness gate |

### ✅ Phase 2 — Earley + SPPF

All six sprints complete. LALR compliance 512/512 = 100%; Earley basic bank
211/211 (clean); dynamic-lexer bank 454/454 = 100% (clean). #32 (XFAIL burndown)
is ✅ done — all three clusters cleared: cluster 1 ("nested `_ambig` through a
transparent `_rule`/EBNF helper") by porting Lark's `AmbiguousExpander`; cluster 2
(`%ignore`-of-content) by re-anchoring the dynamic scanner's ignore carry-over
through the forest's global node index so carried derivations *merge* rather than
shadow each other (materializing any deferred Joop-Leo path first); cluster 3
(`dynamic_complete` resolve tie-break) by a split-point tie-break in
`sorted_families`, gated to the dynamic lexer, that restores Python's
earliest-split-first segmentation order (lark-rs's EBNF helper nodes otherwise
reverse it via LIFO completion). #33 (de-recurse forest walk) is ✅ done — the
forest→tree walk (value assembly, lazy priority sum, `_ambig` dedup keying) runs
on explicit heap frames instead of native recursion, so the dedicated 256 MB-stack
thread is gone and the walk's native-stack use is O(1) in forest depth (pinned by
`tests/test_earley_stack.rs`, which replays deep transparent/nested chains on a
512 KB thread; this also unblocked WASM (#47), which has no `std::thread`).
#151 (its follow-up) is ✅ done — `Tree`'s `Drop`/`Clone` are manual worklist
impls (in both `tree.rs` and the standalone runtime's own `Tree`), closing the
last input-depth recursion: the compiler-derived glue, which bit when a caller
dropped/cloned a deep result tree or the walk discarded a deep already-built
child (resolve-mode family rollback).
#35 (strict regex-collision) is ✅ done — a
`regex-automata` product-construction emptiness test backs the strict-mode check.
#31 (Earley perf gate) is ✅ done — the shared bench harness re-runs the
unambiguous workloads under `parser='earley'` and reports the Earley/LALR ratio
trend (see `BENCH.md`); the constant-K ceiling was downgraded to deferred and the
underlying super-linearity has since been removed by the Joop-Leo work (#58).

| Component | Status | Notes |
|-----------|--------|-------|
| Ambiguity test harness (Sprint 0) | ✅ | Earley oracles + `_ambig` set-matcher + Earley compliance bank (147 grammars), self-activating the moment the frontend builds |
| Earley recogniser | ✅ | Sprint 1: predict/scan/complete over `SymbolId`. Now reimplemented on top of the Sprint-2 chart (`recognize` = "did the start node build?"), so it accepts exactly what `parse` parses. Verified by `test_earley_recognizer` |
| SPPF forest construction | ✅ | Sprint 2: Elizabeth Scott's binarized SPPF (symbol / intermediate / packed nodes, arena-allocated by `NodeId`, held-completion nullable handling). #56: per-column `waiting` index removes the completer's O(column) origin rescan. **#58: Joop-Leo** deterministic-reduction-path optimization — the completer records a transitive per column and jumps to the topmost item instead of cascading, with a lazy spine reconstruction (`load_leo_paths`) over a forest-global `(key,start,end)` node index so the SPPF stays byte-identical. Collapses hand-written right recursion (`a: X a \| X`) from O(n²) completed items to a flat per-byte completer scan — lark-rs is now *faster than the Python oracle here*, which never finished Leo (its completer references a nonexistent field; see lark-parser/lark#397). Restricted to strict right recursion (recognized symbol is the rule's last); nullable-tail right recursion falls back to the regular completer |
| Forest → tree conversion | ✅ | Sprint 2: `Transformer` walks the SPPF and reuses `TreeBuilder::assemble`; `ambiguity='resolve'` picks the highest-priority derivation (Lark's `ForestSumVisitor` order). Verified ≡ LALR on every unambiguous oracle by `test_earley_parity` |
| `ambiguity='explicit'` | ✅ | Sprint 2: emits `_ambig` forests; curated cases pass, bank 211/211 (clean — the `AmbiguousExpander` port lifts an ambiguous transparent `_rule`/EBNF-helper child's ambiguity up into the parent) |
| Dynamic lexer | ✅ | Sprint 5: scanning folded into the Earley loop (`xearley.py` port) — terminals tried at each position are exactly those the parser predicts. `LexerType::Dynamic`. Delayed-match buffer for variable-length tokens + `%ignore` carry-over. Terminal priorities feed the forest sum (the basic lexer consumes them in its ordering; the dynamic lexer does not). Bank 454/454 = 100% (clean) |
| `dynamic_complete` | ✅ | Sprint 5: `LexerType::DynamicComplete` — also explores every shorter tokenization, so all segmentations are considered |

### ⬜ Phase 3 — Full Feature Parity

| Component | Status | Notes |
|-----------|--------|-------|
| Complete `common.lark` stubs | ✅ | The full upstream `common.lark` is bundled (`src/grammars/common.lark`) and parsed through lark-rs's own terminal-algebra loader, not a hand-transcribed regex table — so common terminals can't drift. Added `CR`/`LF`/`SQL_COMMENT` + the `_EXP`/`_STRING_*` helpers; one documented lookbehind adaptation for `ESCAPED_STRING`. Pinned by `test_common.rs` |
| `%import` from file path | ✅ | Relative imports (`%import .module (X, ...)`) resolve against the importing grammar's directory (`LarkOptions.base_path`), load through `load_grammar`, and copy the requested terminal/rule — a rule pulls in its dependency closure, mangled under the module name (Python's `_get_mangle`). Pinned by `test_imports.rs` (oracles in `fixtures/oracles/imports/`, grammars under `tests/grammars/imports/`) |
| `%declare` semantic action | ✅ | `%declare _INDENT _DEDENT` registers pattern-less terminals (`TerminalDef::declared`): interned + reserved a parse-table column, filtered out of every scanner (`basic_lexer_conf`), injected by a postlex hook. Pinned by `test_indenter.rs` |
| Indenter / postlex | ✅ (all parsers; LALR: basic + contextual lexer) | `LarkOptions.postlex: Option<Indenter>` on **both** the basic and the contextual (default) lexer for LALR, and on the basic lexer for **Earley and CYK** (#78). Basic lexer: materialize the stream, `Indenter::process` rewrites it (INDENT/DEDENT injection, paren-depth suppression, tab expansion, end-of-input dedent flush — a token-for-token port of `lark.indenter.Indenter`), then the parser replays it. **#67: contextual lexer** — the lazy per-state lexer can't be materialized up front, so the indenter runs as a streaming `TokenSource` adapter (`PostlexContextual`) inside the pull loop, driving the shared `IndenterStream` core so it injects a byte-identical stream; the NL terminal is forced into every state's scanner via `always_accept` (Python Lark's `PostLex.always_accept`). Pinned by `test_indenter.rs`, which replays the `indent`/`indent_paren` oracles under both lexers **and** adds `indent_context` — a grammar where the contextual lexer's state-narrowing is load-bearing (`NAME`/`VALUE` overlap, basic lexer provably can't parse it) *while* postlex injects INDENT/DEDENT, so the two mechanisms are pinned together, not just for parity. **#78: Earley + CYK** — both run the hook over the materialized basic-lexer stream (the same wiring as the LALR basic path; Python Lark's `lexer='auto'` resolves to `'basic'` for Earley + postlex). Each is pinned by its own oracle fixture generated under that parser (`test_indenter.rs::test_indenter_earley_cyk_oracle`, `indent*/earley_cases.json` + `cyk_cases.json`). The dynamic lexer refuses postlex with Python's own error (scanning is folded into the parse loop — no stream to rewrite). The cross-engine bench gains an Earley `python_sm` row (the real `python.lark` + Indenter, input-bounded because Python Lark's Earley measures ~0.001 MB/s on it). **#69: a general trait-object postlex** (beyond the built-in `Indenter`) is the remaining follow-up |
| Grammar standard library | ✅ | Beyond `common.lark`, lark-rs bundles every grammar Python Lark ships under `lark/grammars/` — `python.lark`, `unicode.lark`, and `lark.lark` — under `src/grammars/`, resolvable via the same `%import <lib>.<X>` directive. The files are **verbatim** copies (one exception, `common.lark`'s `ESCAPED_STRING`): the loader's bundled-library path parses each through lark-rs's own loader and copies the requested terminal/rule closure, mangled under the module prefix (`python__HEX_NUMBER`). A handful of their terminals use lookaround. The active **lexer DFA plan** (`docs/LEXER_DFA_PLAN.md`) lowers the supported bounded shapes into the DFA — **every bundled lookaround terminal now lowers**: `STRING` via the M4 opening-guard splice, `lark.REGEXP` via the Stage-B regex-literal idiom, and `python.LONG_STRING` via the Stage-B long-string idiom (grammars stay verbatim, not rewritten; see the routing section below and `docs/LEXER_DFA_STATUS.md`). *Historical:* the earlier **lookaround-elimination** plan (`docs/LOOKAROUND_ELIMINATION_PLAN.md`) milestone E2a added an *equivalence-proof harness* but changed no grammar; it found `LONG_STRING` and the block-comment shape *provably* rewritable lookaround-free (`long_string_match_length_equivalence`, `block_comment_match_length_equivalence`, once deferred to "E4") and `STRING` *irreducible* by a grammar rewrite (its `(?!"")` rejects `""""` while accepting `"" ""`, a distinction lost once `%ignore` drops whitespace — `string_lookaround_free_rewrite_is_not_equivalent`). The DFA plan supersedes that rewrite framing (it lowers in the lexer rather than editing grammars), but the behavioral findings stay pinned in `tests/test_lookaround.rs`. Pinned by `tests/test_stdlib.rs` (oracles in `fixtures/oracles/stdlib/`). SQL/C/Lua are *not* bundled — upstream distributes them as separate packages, not under `lark/grammars/` |
| Standalone parser gen | ✅ (Rust) | `lark-rs generate-parser --grammar foo.lark --output parser.rs` (`src/bin/generate_parser.rs`) emits a self-contained Rust LALR parser depending only on `regex` + std, not on lark-rs (#42). `src/standalone/mod.rs` runs the normal pipeline once and bakes the `ParseTable` (sparse ACTION/GOTO), per-rule tree-shaping flags, the symbol-name table, and the `ScannerPlan` (alternation order + `unless` retype) into one `static DATA: GrammarData`. The driver (basic lexer + LALR + tree-shaping) lives in `src/standalone/runtime.rs` — a **real compiled, type-checked, unit-tested module** that is `include_str!`d into each generated parser, not a hand-copied text blob. Both drift vectors are shared by construction: the lexer recipe is the **same** `lexer::scanner_plan` the in-process `Scanner::build` uses, and the driver is the one compiled module. So a generated parser is byte-identical to lark-rs — pinned two ways: `test_standalone.rs` (committed `tests/standalone/*.rs` fixtures `include!`d + run vs the live oracle, plus a determinism/freshness gate), **and** a compliance-bank replay (`standalone::tests::standalone_compliance_bank`, #86) that runs the shared `runtime` over the **full strip-mined Python-Lark bank** — 508/512 cases agree with the captured oracle (the 4 XFAILs in `standalone_xfail.json` are basic-lexer-incompatible grammars, e.g. `"a"i "a"`, whose contextual-captured oracles Python's own *basic* lexer cannot reproduce either — verified; allow-listed via `LARK_STANDALONE_WRITE_XFAIL=1` with the same burndown discipline as the LALR/Earley banks). Value is dependency footprint + Python-`standalone` parity, **not** throughput (still table-interpreted) or `no_std` (runtime regex compile); see the module docs. Limitations: LALR + basic lexer only, no postlex (rejected with a clear error); a grammar with **lookaround terminals** (the bundled `python`/`lark`) is not standalone-able since the baked runtime is pure-`regex`. Follow-ups: Python standalone; the L5 serialized-DFA bake (which makes the lookaround grammars standalone-able); unify the `ParseTable→Rust` emitter with `include_lark!` (#49) |
| Error recovery | ✅ | Panic-mode **single-token-deletion** recovery on the LALR backend (#43). `Lark::parse_with_recovery` (built-in strategy) and `parse_on_error` (custom handler) mirror Python Lark's `on_error` callback — which, with `on_error=lambda e: True`, *is* delete-and-resume (its `interactive_parser.resume_parse()` has already pulled the bad token off the lexer). Same LALR tables ⇒ the surviving stream builds the **same tree**, so it is oracle-gated: `tests/test_recovery.rs` asserts tree + deletion-count parity vs Python (`recovery/cases.json`). Returns a `RecoveredTree { tree, errors }` — the partial tree plus the recovered errors (the "error nodes"; an LR value stack has no slot to splice them inline without a yacc-style `error` production, which Lark's grammar syntax lacks, so they sit alongside, exactly as Python's recovery does). Recovery lexes with the basic/global lexer so out-of-context-but-valid tokens are deletable; a `$END` error returns a best-effort partial instead of aborting (Python re-raises). Plan: [`PHASE_3_RECOVERY_PLAN.md`](PHASE_3_RECOVERY_PLAN.md). Follow-ups: character-level recovery, Earley/CYK/postlex recovery |
| CYK parser | ✅ | `parser='cyk'` (#44). Faithful port of Python Lark's `cyk.py`: CNF conversion (TERM lifts non-solitary terminals into `__T_` wrappers, BIN binarizes >2-symbol rules via `__SP_` splits, UNIT eliminates non-terminal unit rules recording the skipped chain) + an O(n³) DP that keeps the lightest derivation per span/non-terminal, then a CNF revert that feeds the shared `TreeBuilder` — so an unambiguous parse is byte-identical to LALR/Earley. lark-rs's nullable `*`/`?`/`+` helpers are transparent, so a reachability prune + ε-removal pass (duplicate each rule over its nullable occurrences; refill omitted transparent positions with an empty splice) reproduces Python's ε-free EBNF expansion without changing the tree; a nullable *non-transparent* rule is a genuine ε-rule CYK can't model and is rejected at build time, matching Python. Uses the basic lexer (no parser-state lexer, like Earley). Pinned by `test_cyk_compliance.rs` — the CYK bank (TestCykBasic) is **124/124 = 100%** oracle agreement (0 XFAIL) — plus inline parity/ambiguity/EBNF unit tests in `cyk.rs`. **#87: a deterministic cubic-envelope scaling gate** (`test_cyk_scaling.rs`) keys on the `cyk_table_steps` work counter and asserts the table fill stays flat per n³ on a densely ambiguous grammar (`s: s s \| "a"`), so a complexity regression in the CNF conversion or DP is caught — the CYK analog of the Earley scaling net |

### ⬜ Phase 4 — Distribution

| Component | Status | Notes |
|-----------|--------|-------|
| PyO3 Python binding | ✅ | `lark-rs/python/` — a `maturin`/PyO3 crate exposing `Lark` / `Tree` / `Token` with Python Lark's kwargs (`parser`, `lexer`, `start`, `ambiguity`, `propagate_positions`, `keep_all_tokens`, `maybe_placeholders`, `strict`, `g_regex_flags`). `Token` is `str`-like; errors map to `LarkError`/`GrammarError`/`ParseError`. `abi3-py38` wheel via `maturin build`. Round-trip parity pinned against the Python-Lark oracle by `python/tests/test_roundtrip.py` |
| WASM target | ✅ | `lark-rs/wasm/` (#47) — a `wasm-bindgen` crate (excluded from the workspace, like `python/`) packaged by `wasm-pack` into an npm package (`npm run build` → `pkg/` for Node, `pkg-web/` for browsers). API mirrors the PyO3 binding (`new Lark(grammar, {parser, lexer, start, ...})`, errors with `name` = `GrammarError`/`ParseError`); `.parse()` returns the tree as a plain JS object in the **oracle JSON shape**, so JS tests compare directly against committed Python-Lark fixtures. Serialization is an explicit-stack walk and `Tree`'s `Drop`/`Clone` are iterative (#151), so deep parse results survive WASM's ~1 MB stack — pinned by a 50k-deep smoke case. Bundled `%import` libraries work (in-memory), and relative file `%import` resolves through `LarkOptions.import_sources` / the JS `importSources` option — an in-memory map of virtual `/`-separated paths to grammar text, nesting through virtual directories exactly like sibling files on disk (pinned identical to the filesystem mode by replaying the same imports oracle from memory, `tests/test_imports.rs`). Without it, a file `%import` fails with the usual `ImportNotFound`. Gated by `tests/wasm/` (JS smoke tests vs the JSON oracle corpus) in its own CI job (`wasm-binding`). **`wasm/demo/`** is a static, mobile-friendly browser playground over the web build (grammar editor + example bank + `importSources` virtual files + live tree/JSON output), deployed to GitHub Pages by `.github/workflows/demo-pages.yml`; its example bank is pinned by `tests/wasm/demo_examples.test.mjs` under the same `npm test` |
| C API | ✅ | `lark_h` crate (#48): `#[no_mangle]` surface (`lark_new`/`lark_parse`/`lark_tree_*`/`lark_free`) + committed `lark.h` + C smoke test. lark-rs is now a workspace so `cargo test --all` covers it |
| `include_lark!` proc-macro | 🟡 | Compile-time grammar validation (#49). `lark_proc/` crate: `include_lark!("grammars/x.lark")` reads + validates the grammar through the real `Lark` loader at `cargo build`, so a bad grammar is a compiler error (file/line, attributed to the macro span), and generates a typed `XParser` struct with `parse(&str) -> Result<ParseTree, ParseError>`. The grammar source is embedded; the `Lark` is built once per thread (`thread_local!`, since `Lark` is not `Sync`). Pinned by `lark_proc/tests/include_lark.rs` (runtime parsing) and `lark_proc/tests/compile_fail.rs` (a malformed grammar fails `cargo build` with the validation error attributed to the macro span — the headline #49 guarantee, regression-netted). Follow-up: bake the LALR `ParseTable` into `const` data so no table construction happens at runtime (regex lexer still compiles patterns at runtime regardless) |
| Benchmarks vs Python Lark | ✅ | #50: `cargo bench --bench vs_python_lark` — JSON / Python / SQL through both engines, byte-identical inputs, prints MB/s + speedup (~4–6× on the reference box). Results in `BENCH.md` |

---

## Lookaround / Lexer-DFA Routing — Full Detail

> Summary and the load-bearing instructions live in `CLAUDE.md` ("Key Design
> Decisions & Gotchas"). This is the complete record.

**`regex` crate has no lookahead or backreferences.** Some Python Lark grammars rely
on lookaround (the bundled `python.lark`/`lark.lark` do: `STRING`'s
`(?!"")…(?<!\\)(\\\\)*?` guards, `DEC_NUMBER`'s `(?![1-9])`, `lark.OP`/`REGEXP`).
**Direction (2026-06-08): [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md)** is the
active umbrella — build the combined scanner on a `regex-automata` DFA and **lower** the
bounded lookaround into it (a DFA, *not* PR #110's Pike-VM), so every terminal lexes
single-pass and the `python`/`lark` grammars become bakeable. **Status (L2):** all three
lowering shapes have landed behind the `LexerBackend::Dfa` engine — **trailing** boundary
(M1, `OP`/`DEC_NUMBER`'s `(?![1-9])`/`(?![a-z])` guarded accept), **leading** boundary
(M2, a match-start precondition), **bounded lookbehind** (M3, a backward guard at a
*fixed* char-offset), the **`python.STRING` opening-guard splice** (M4 —
`src/lookaround/lower.rs::recognize_string_idiom`), and the **Stage-B delimited-token
idioms** — `lark.REGEXP` (`recognize_regexp_idiom`) and `python.LONG_STRING`
(`recognize_long_string_idiom`). M4 is the marquee L2 piece:
`STRING`'s
`(?!"")` after the variable-width prefix (`[ubf]?r?|r[ubf]`) + the opening quote is an
internal/variable-position leading boundary, lowered by normalizing the lazy escaped body
`.*?(?<!\\)(\\\\)*?<q>` to its proven greedy character-class equivalent (which *absorbs*
the `(?<!\\)` lookbehind) and reducing `(?!"")` to an empty/non-empty arm split with a
trailing `(?!")` guard on the (prefix-free) empty arm. The REGEXP idiom is the second
audited delimited-token lowering: the internal `(?!\/)` after the opening slash reduces
*exactly* to a non-empty-body bump on the lazy repetition (`*?` → `+?`) because the close
and every body alternative start with disjoint chars, so REGEXP lowers to one unguarded
branch whose lazy/priority match end the leftmost-first plain engine reproduces natively
(gated by `tests/test_regexp_splice.rs` canaries — `//` is a lex error, `/a//` never
swallows the second slash, the dangling-escape close `/a\/b` → `/a\/` — plus the
generative equivalence + `*?`-mutant and a state-pruned Route-1 proof). The LONG_STRING
idiom is the third: the lazy body + escape-parity close `.*?(?<!\\)(\\\\)*?"""` is
normalized to lazy escape-pair items `(?:[^\\<nl>]|\\.)*?"""` (a backslash can only start
a pair, so item boundaries fall exactly at the even-parity positions the lookbehind
demanded; the kept lazy `*?` picks the first valid triple — no multi-char delimiter
automaton needed), two unguarded per-arm branches (gated by
`tests/test_long_string_splice.rs` canaries — `""""""` is one empty token, `"""\"""` is a
lex error, docstrings span newlines — plus the exhaustive dotall backend differential,
generative equivalence + parity/two-quote/greedy mutants, and a state-pruned Route-1
proof). **The flag-wrapper strip makes the idioms real on the engine path**: the loader
bakes terminal `/…/is` flags into the pattern (`(?is:…)`, `PatternRe.flags = 0`), so
before `strip_whole_pattern_flag_wrapper` (in `DfaScanner::build`) the wrapped
`python.STRING` silently rode the `Unsupported` compat fallback at runtime — invisible to
the differential because the fancy reference agreed; now the wrapper is stripped into the
flag bitset before routing and re-applied to every lowered branch/guard, `g_regex_flags`
DOTALL is threaded the same way, and
`lexer::tests::dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe` pins that the
three bundled idioms build with **zero** fancy side-probes. The `Dfa` backend
is gated
byte-identical to the `fancy-regex` `Scanner` over the compliance bank + JSON corpus +
python/lark files + a generated lookaround population including STRING's nested shape and
the REGEXP/LONG_STRING idioms
(`tests/test_scanner_differential.rs`, 0 divergences, all bundled idioms *lowered*) and
per-shape
generative-equivalence + Route-1 proofs (incl. the real nested STRING shape) + mutation
meta-tests (incl. the drop-the-`(?!"")`-guard canary `tests/test_string_splice.rs`:
`""""` is a lex error, `"" ""` is two empty STRINGs). `LexerBackend::Dfa` **is now the default**
(`LexerBackend::default()` / `LarkOptions.lexer_backend`): the L0 differential oracle is
0 divergences over the full bank + JSON + python/lark corpora, so the swap is
correctness-identical, and it is faster on the all-plain common path
(`benches/lex_backends`, `BENCH.md`); `LexerBackend::Regex` stays selectable and the
differential keeps both engines gated against each other.

**Current routing (master) — L4 landed, no fallback engine.** Each terminal takes one of a
**typed** `classify::LoweringRoute` (`route_terminal_dotall`, matched by the single
refusal seam `lexer::route_fancy_only_terminal` after the flag-wrapper strip + the
vacuous-`(?:…)`-wrapper normalization): *Plain* (no lookaround → the DFA), *Lowered* (a
supported bounded assertion → DFA branches + guard tables — M1/M2/M3/M4, the Stage-B
REGEXP/LONG_STRING idioms, and guarded bases proven by the exact `is_leftmost_longest`
semantic gate, e.g. `python.DEC_NUMBER`; **every bundled lookaround terminal is here**),
or a **categorized build error** (`GrammarError::LookaroundScope`) under the
two-category scope taxonomy of **`LOOKAROUND_SCOPE.md`**: *Unsupported* →
`Scope::OutOfScope` (by-design non-goals: general internal lookahead — the audited
idioms are the named growth path; variable-width lookbehind — Python `re` rejects it
too; backrefs/backtracking-only syntax — the named parity break) and *Declined* →
`Scope::NotYetImplemented` (clean conservative refusals that double as promotion
tripwires: variable-offset lookbehind, non-realizable guarded bases, VERBOSE mode —
whether a `(?x:…)` wrapper or global `g_regex_flags`).
The contract is scoreboarded end-to-end by `tests/test_lookaround_scope.rs` (with an
exhaustiveness meta-test over every refusal variant) and enforced identically on every
engine path — the combined scanners, the Earley dynamic `DynamicMatcher` (per-terminal
`LoweredTerminalMatcher`s), and `unless` retyping. **`fancy-regex` is NOT a runtime
dependency**: default builds carry zero fancy code (`cargo tree -e normal`); it remains
a dev-dependency oracle plus the default-OFF TEST-ONLY `fancy-oracle` feature (the
`Regex` reference backend's historical probes for the L0 differential, run in CI as
`cargo test -p lark-rs --features fancy-oracle`). L5 (bake the scanner static) is now
unblocked; see `LEXER_DFA_STATUS.md` / `LEXER_DFA_PLAN.md`.

> **Historical (lookaround-*elimination* plan, superseded by the DFA plan).** The earlier
> `LOOKAROUND_ELIMINATION_PLAN.md` (now Phase 1 of the DFA plan) classified terminals
> into a reducible Tier-E and an irreducible G-tier (see
> `TERMINAL_REDUCTION_DIAGNOSIS.md`); milestone **E2a** built an equivalence-proof
> harness but **changed no grammar**, recording that `LONG_STRING` was *provably* rewritable
> lookaround-free (the old plan deferred that rewrite to "E4") while `STRING`'s `(?!"")` was
> *proven irreducible* by a grammar rewrite. The active DFA plan supersedes that framing:
> grammars stay **verbatim**, and `STRING`, `REGEXP`, and `LONG_STRING` are all
> **lowered** into the DFA (the M4 opening-guard splice and the two Stage-B
> delimited-token idioms — not rewritten, not routed to fancy). E2a's
> `long_string_match_length_equivalence` finding became the committed proof basis for the
> LONG_STRING idiom's body normalization. The behavioral findings are still pinned by
> `tests/test_lookaround.rs`.

**The historical `\G` anchoring lives on only in the `fancy-oracle` test feature.** A
fancy terminal probed at each offset with `find_from_pos` is an *unanchored forward
search* — left as-is a sparse fancy-routed terminal was O(n²) over the input (a 124 KB
Python file took ~177 s before the `\G` start-of-search anchor was prepended at build,
restoring linear-per-byte lexing). Since L4 no runtime fancy probe exists; the `\G`
probe survives solely as the reference behavior of the `Regex` backend under the
TEST-ONLY `fancy-oracle` feature (the L0 differential's independent oracle), where the
anchoring is still load-bearing. The lowered DFA path is linear by construction
(`cargo bench --bench redos` characterizes it; `test_lexer_scaling` gates it). One
terminal — `common.lark`'s `ESCAPED_STRING` — keeps its hand-written lookaround-free
adaptation (it's the hottest terminal in the library and was already linear on the pure
`regex` engine); it is the single standing exception to "verbatim upstream."

---

## Open Work

All open tasks are tracked as GitHub issues. #39 (`%import` file paths), #45
(`%declare`), #41 (Indenter/postlex, basic lexer), #67 (postlex over the
contextual lexer), #35 (strict regex-collision), #44 (CYK parser), #42 (standalone
parser — Rust variant), #40 (grammar stdlib), #32 (Earley XFAIL burndown), and #43
(error recovery) are ✅ done. Phase 3 is feature-complete; remaining work is the
follow-ups below.

Follow-ups: a Python standalone emitter (#42); and the lookaround/throughput rework in
**[`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md)** (active). Today the standalone
runtime emits a pure-`regex` parser, so a grammar with lookaround terminals (the bundled
`python`/`lark`) is not yet standalone-able — that bakeability is the explicit payoff of
the DFA plan's final phase (a serialized `regex-automata` DFA replaces the baked
`ScannerPlan` alternation).

Low-priority API generality: #69 (general trait-object postlex beyond the built-in
`Indenter`) — split out of #67; the `Indenter` covers the common case, so this is
not a parity gap on any shipped grammar.

Phase 4 distribution (#46–#50) follows after Phase 3 is substantially complete.

---

## Wild-Grammar Bank — Findings

> How the bank works (vendoring, oracles, XFAIL discipline, the alt-grammar bar)
> is documented in `CLAUDE.md` and [`../tests/wild/README.md`](../tests/wild/README.md).
> These are the current results.

Findings (updated 2026-06-12, post-fence-idiom round — the xfail set encodes them,
and each fixed root cause is pinned in distilled form by `tests/test_wild_gap_pins.rs`):
**189/257 inputs agree, 72 XFAIL, 4 grammars not building.** Every remaining failure
is an **engine-scope refusal** — an internal-lookahead/backtracking construct the
lexer-DFA routing *deliberately* rejects (`LOOKAROUND_SCOPE.md`). A project may
carry an `alt_grammar` in its `meta.json`: a workaround edit replayed when the
original fails to build, proving "a valid edit exists in grammar land." The bar is
strict and **structurally enforced**: the alt must build and be **tree-identical to
the original grammar's Python oracle on every input** — its
`build-alt:`/`parse-alt:`/`panic-alt:` failure namespaces are *not xfail-able*
(`test_wild.rs` asserts none appear in `xfail.json`, and `LARK_WILD_WRITE_XFAIL`
never writes them), so a divergent alt fails the build and must be removed, not
allow-listed. A corpus-coincidental edit (matches the project's few inputs but is
semantically divergent) does not qualify — that would instill false confidence and
lean on honest caveats instead of tests. No current project has a qualifying alt;
the investigated near-misses are recorded in each project's `meta.json`
(`alt_grammar_finding`).

**Cleared by the burndown round** (the wild bank's first payoff):

* **vyper** (build + 7/7): plain `(a|b)` groups now distribute into the parent's
  alternatives at every position (Python's `SimplifyRule_Visitor`) instead of
  materializing `__anon_group_*` helpers whose unit alternatives duplicated other
  rules' RHS and collided as unresolvable reduce/reduce.
* **matter_idl** (8/8) and **pyquil** (6/6, `test1.quil`'s `1/sqrt(2)` included):
  a `"keyword"i` literal is now a `PatternStr` with the `i` flag attached (Python
  keeps the type, only attaching the flag), so it joins the lexer's `unless`
  keyword retyping — case-insensitively, via per-keyword `^(?i:…)$` matchers — and
  sorts with string-pattern width. The embed rule mirrors Python's flag-subset
  test: a `"kw"i` under a case-sensitive regex terminal stays in the alternation.
  (This also made the basic lexer agree with Python's on the standalone bank's
  `"a"i "a"` case — its accidental pass via a retype Python never does became an
  honest basic-lexer-incompatible xfail, `standalone_xfail.json`.)
* **cel 40/40** ✅: the upstream `{4-8}`-for-`{4,8}` quantifier typo in
  `BYTES_LIT`/`STRING_LIT`/`MLSTRING_LIT` is **patched in the vendored copy**
  (policy: we do not file upstream bugs — a wild grammar bug is either left
  xfail'd or patched locally and recorded in `meta.json` `local_patches`; the
  oracle trees were byte-identical before/after, so only lark-rs's build was
  affected).
* **dotmotif 22/22** ✅ (Earley + dynamic lexer): three independent gaps —
  comment lines *between* the `|` alternatives of a multi-line rule (the loader
  now lets a full-line comment swallow its leading newline, like lark.lark's
  `COMMENT`), the **short-string idiom** `<q>.+?(?<!\\)(\\\\)*?<q>`
  (`FLEXIBLE_KEY` — audited delimited-token idiom #4, `LOOKAROUND_SCOPE.md`),
  and the Python-dialect `\<`/`\>` escapes (`OPERATOR`'s `[\!=\>\<]`/`\<\>` —
  Python reads literals, the regex crate reads word-boundary assertions; the
  loader now normalizes the two divergent escapes to bare chars).
* **mappyfile 8/8** ✅ (was an L4 internal-lookahead refusal): the
  **vacuous-group splice** (`(?:X) ≡ X`, applied recursively/splicing in
  `classify.rs::unwrap_vacuous_groups`) exposes the trailing guard the loader's
  terminal-reference composition buried (`SIGNED_INT: ["-"|"+"] INT` →
  `(?:\-|\+)?(?:[0-9]+(?![_a-zA-Z]))`), and the M1 path + semantic gate lower it.

**Engine-scope refusals** — wild grammars L4 *deliberately* rejects
(`LOOKAROUND_SCOPE.md`), the measured real-world cost of dropping the
backtracking engine (4 of 16 wild grammars, all non-building):

* **hcl2**: the heredoc backreference terminals now lex via the **fence-idiom
  matcher** (idiom #5, `lexer/fence.rs` — the tag-echo recognizer that round
  landed), but the grammar still fails the build on `STRING_LIT`'s internal
  `(?!\${)` lookahead (L4 out-of-scope).
* **gersemi_cmake**: BRACKET_ARGUMENT (`[==[…]==]`, `(?P=equal_signs)`) likewise
  lexes via the fence matcher, and UNQUOTED_ELEMENT's leading `(?!\[=*\[)` guard
  now classifies (unbounded *leading* lookahead is supported) — but the loader
  inlines the elements into `UNQUOTED_ARGUMENT : UNQUOTED_ELEMENT+`, which
  re-internalizes the guard, so the original grammar still fails the build.
  **No qualifying alt grammar is committed** — both investigated edits fail the
  tree-identity bar (recorded in `meta.json` `alt_grammar_finding`): dropping
  the guards changes *Python Lark's own* trees on 3/8 inputs (the guards are
  load-bearing), and hoisting a single leading guard onto `UNQUOTED_ARGUMENT`
  (buildable here via the unbounded-leading lowering) is corpus-identical on
  8/8 — lark-rs-on-edit ≡ Python-on-original, verified 2026-06-12 — but
  provably divergent on inputs like `$[=[x]=]` (internal element boundaries
  after `$`/escape/reference no longer re-check the guard), i.e. tree-identical
  by corpus coincidence, not by construction.
* **synapse_storm**: atomic groups `(?>…)` + recursive subpatterns `(?&NAME)`
  (`regex`-module-only; context-free lookahead — the one genuine
  backtracking-engine case in the bank).
* **miniwdl_wdl**: width-1 internal lookahead inside a repetition
  (`COMMAND1_FRAGMENT`'s `(?:\$(?=[^{])|~(?=[^{])|[^~$}])+`). Investigated for
  idiom #5 and **deliberately not landed**: Python's greedy `(item)+` is a
  *greedy-commit* loop (no backtracking across committed items), so the exact
  lowering needs priority-aware arm disjointification — a longest-valid-accept
  construction provably diverges (e.g. `\\'`: Python commits the `\\\\` arm and
  stops at 2 chars where the longest decomposition reaches 3) — **and** the two
  engines compose the `STRING1_CHAR` alternation in different arm orders
  (lark-rs sorts by source length, putting `\\'` *after* the char class; Python
  Lark puts it before), which is order-sensitive here. Both must be resolved
  together; recorded so the next attempt starts from this analysis.

**Fully passing**: vyper (build + 7/7, LALR + PythonIndenter postlex), matter_idl
(8/8), pyquil (build + 6/6), cel (40/40), dotmotif (22/22), mappyfile (8/8),
lark_lark (the P0 baseline — lark.lark over the 12 real grammar files
`examples/lark_grammar.py` parses upstream, incl. python.lark and a full Verilog
grammar), pylogics_ltl (relative rule imports + trailing-lookahead terminals
through the M1 lowering), mistql (Earley + dynamic lexer), tartiflette,
poetry_markers, poetry_pep508 (file-relative `%import`).

Oracle note: embedded trees are capped at 55 levels (`EMBED_DEPTH_LIMIT`) —
serde_json refuses JSON nested deeper than 128 and a tree level costs ~2 —
deeper trees (CEL's non-collapsed cascade) are digest-verified only.
