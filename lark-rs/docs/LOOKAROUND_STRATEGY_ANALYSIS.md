# Lookaround in lark-rs: strategy analysis and recommendation

*Status: analysis / decision input — not yet a committed implementation plan.*
*Date: 2026-06-07. Context: review of [PR #110](https://github.com/okalldal/lark/pull/110)
("Lexer DFA M1–M3 — lower lookaround to a linear Pike-VM engine, remove fancy-regex").*

## TL;DR

PR #110 replaces the `fancy-regex` backtracking overlay with a hand-rolled,
linear **Pike-VM** that *executes* bounded lookaround at match time. It is well
built and removes a real ReDoS. But it answers the wrong question. The lookaround
in Lark grammars is an inherited Python-`re` **idiom**, not a load-bearing
language feature, and a bounded assertion is by definition regular — so it can be
**eliminated at grammar-load time** and run on the one fast linear engine lark-rs
already depends on (the `regex` crate), instead of *re-implemented* at runtime.

This document works through the question from first principles and backs it with a
two-corpus census of real Lark grammars. The findings:

- **Both engines are linear** (no ReDoS); the contest is average-case speed and
  scope, and on both the load-time-elimination approach wins **structurally**, not
  by tuning.
- **Faithfulness is barely sacrificed.** Every lookaround construct found in the
  wild is *bounded → regular → reducible*. We found **zero** irreducible cases,
  **zero** backreferences, and **zero** variable-width lookbehind across ~40
  distinct real grammars.
- The one nastiest real pattern found (an unbounded-width negative lookahead in
  `strictdoc`) is simultaneously the case the Pike-VM handles *worst* (O(n²)) and a
  case the elimination approach handles *fine* (it is still regular).

**Recommendation (Option 1b):** ship lookaround-free equivalents of the bundled
grammars, add a small load-time pass that rewrites the two dominant reducible
idiom-classes automatically, and reject anything genuinely irreducible with a loud,
actionable build-time error. This wins linearity, speed, and a far smaller
maintenance surface, while preserving observable behavior for essentially every
grammar that works in Python Lark today.

---

## 1. The problem

The `regex` crate (and `regex-automata`) deliberately reject lookaround
(`(?=…)`, `(?!…)`, `(?<=…)`, `(?<!…)`) and backreferences, because they break the
linear-time finite-automaton model. A handful of Lark's bundled grammars use
bounded lookaround:

| Terminal | Grammar | Assertion(s) |
|---|---|---|
| `STRING`, `LONG_STRING` | `python.lark` | `(?!"")`/`(?!'')`, `(?<!\\)(\\\\)*?` |
| `REGEXP`, `OP` | `lark.lark` | `(?!\/)`, `(?![a-z])` |
| `DEC_NUMBER` | `common.lark` | `(?![1-9])` |
| `MULTILINE_COMMENT` | `verilog.lark` (examples) | `\*(?!\/)` |

lark-rs previously routed these terminals to `fancy-regex` (a backtracking engine).
That carried a genuine ReDoS in `lark.REGEXP` and a "bail-to-wrong-answer" risk if
the backtrack limit is exceeded.

PR #110's response: write a linear Pike-VM that lowers each bounded assertion and
executes it at match time, and delete `fancy-regex`.

## 2. Are we re-implementing Python's regex engine?

Partly — and the part that matters is brittle.

Both `fancy-regex` and PR #110's engine are **hybrids**: they lean on the `regex`
crate for the regular parts and hand-roll only what `regex` can't do. So PR #110 is
not a from-scratch regex engine. The genuinely unbounded, bottomless part of a
regex engine — Unicode classes (`\w`, `\d`, `.`, case folding, property tables) —
is delegated to `regex`. What is hand-rolled is the *structural* machinery:
quantifier greedy/lazy priority, alternation order, anchors, assertion gating,
flag scoping, fixed-width analysis, and a bytecode VM.

That structural core is a closed, textbook artifact (Thompson/Pike/Cox). The
*brittleness* is not the engine — it is the **parity contract**: to be the "Python
Lark oracle," the engine must match CPython `re` byte-for-byte across the long tail
of corner cases (empty matches, `{,n}` vs `{0,n}`, nested-quantifier priority,
anchor edge behavior, flag inheritance, the exact escape set). Each divergence is a
latent silent mis-parse, and there is no differential fuzzer against CPython `re` to
bound that tail. (The review of PR #110 already found two cracks: an unbounded
lookahead falling back to O(n²), and `(?i)`-bodiless-groups rejected.)

### Key differences from `fancy-regex`

| Decision | `fancy-regex` | PR #110 Pike-VM |
|---|---|---|
| Core model | backtracking | linear NFA simulation |
| Worst case | exponential (ReDoS), capped by a backtrack limit | linear; no cap needed |
| Capability | backreferences, atomic groups, broad lookbehind | bounded lookaround only; **no** backrefs; fixed-width lookbehind |
| Delegation to `regex` | whole regular spans, at DFA speed | only single-char classes, one `regex` match per char |
| Failure mode | limit-exceed → `Err` (caller can mis-handle → wrong answer) | unlowerable construct → build-time error |

The surprising one is **delegation granularity**: `fancy-regex` runs long regular
runs as compiled `regex` DFAs and only touches its VM at the lookaround seams;
PR #110 runs the *entire* match loop itself and calls `regex` only to test one
character at a time. So in the matching loop, **PR #110 re-implements *more* of the
engine than `fancy-regex` does** — and pays a per-char constant-factor cost for it.

## 3. The requirement axes

"Best of both worlds" is only meaningful once the competing requirements are named:

1. **Linear / no ReDoS** (safety)
2. **Faithful Python-`re` behavior** (correctness as oracle)
3. **Verbatim upstream grammar text** (drop-in compatibility)
4. **Small maintenance surface**
5. **Average-case speed**
6. **WASM / C / standalone bakeability**

`fancy-regex` gives 2,3,5,6 but not 1,4. PR #110's Pike-VM gives 1,2,3,6 but
sacrifices 4 and 5 (and is leaky on boundedness). The question is whether anything
gets **1 + 4 + 5 without losing 2 + 3.**

## 4. The options

- **Option 0 — keep `fancy-regex`.** Rejected: ReDoS and bail-to-wrong-answer.
  (Note: the "can't go to WASM/C" argument is weak — `fancy-regex` is pure Rust.
  The honest sole justification is ReDoS.)
- **Option A — PR #110's Pike-VM** (execute lookaround linearly at match time).
- **Option 1 — eliminate lookaround at grammar-load time** so all matching runs on
  the `regex` crate:
  - **1a (manual):** rewrite the bundled terminals; reject other lookaround.
  - **1b (automatic):** a load-time pass that rewrites the *reducible* class of
    bounded assertions to regular form, with loud rejection for the irreducible
    tail.

### The trap to avoid: "just compile to a DFA"

The obvious "best of both" — compile the whole terminal to a single DFA via
intersection/complement and take the longest match — is **wrong** for these
grammars. A DFA gives longest-match / recognition; Python's `STRING` uses **lazy**
`.*?` (shortest), and the assertion selects the correct closing quote. A
longest-match DFA would swallow everything to the last quote in the file. So you
genuinely need greedy/lazy **priority** semantics — which is exactly what a Pike-VM
provides. **PR #110's core engine choice is therefore defensible.** The flaw is not
the Pike-VM; it is *executing* lookaround at runtime when the idiom can be
*eliminated* at load time.

## 5. Speed (axes 1 and 5)

Both options are linear, so axis 1 is a tie. Axis 5 goes to Option 1, and the gap
is **structural**, at two levels:

**Micro (within one token).** A rewritten lookaround-free pattern compiles to one
`regex` automaton: a table-driven, byte-at-a-time pass with literal prefilters and
SIMD. The Pike-VM pays thread-list/ε-closure bookkeeping, per-span dispatch, and
re-entry at every assertion seam — even if each span were a compiled DFA.

**Macro (how the lexer drives it) — the decisive level.** A lookaround-bearing
terminal **cannot join the combined scanner alternation** (the combined `regex` can't
express the assertions), so it is probed *individually, anchored, at every token
boundary*. A rewritten terminal is plain regular and rejoins
`(?P<g>…)|…`, riding the single combined DFA pass. This is the whole reason lexers
combine terminals into one regex: **a DFA matches N alternatives in one pass; the
side-probe approach matches them in N passes.** With `k` lookaround terminals and
`T` tokens, Option A adds ~`O(k·T)` extra anchored probes that Option 1 gets for
free.

**Can Option A reach Option 1's speed? Only by degenerating into Option 1** — i.e.
by expressing the terminal without assertions so it can fuse back into the combined
scan, which *is* the elimination rewrite. Where elimination applies (all bundled
terminals), Option A's ceiling is Option 1, reached only by becoming Option 1.

Ranking on axis 5: **Option 1 (combined DFA) > Option A improved (span-delegating
Pike-VM) > PR #110 as written (char-at-a-time membership).**

## 6. Faithfulness (axes 2 and 3) — decomposed

"2/3" is not one scalar. Split it:

- **Axis 2 = behavior:** same tokens/tree as Python Lark.
- **Axis 3 = artifact:** byte-identical bundled `.lark` files.

**Axis 3 mostly collapses into axis 2 and becomes invisible.** When a user writes
`%import python.STRING`, they load *lark-rs's* bundled grammar; they never author or
read that file. If our rewrite is behavior-identical, the fact that our internal copy
differs from upstream is unobservable. Axis 3's irreducible residue is therefore
narrow: (a) maintainer-side upstream-sync ergonomics, and (b) the rare user who
pastes the *entire* upstream grammar as their own source.

So the analysis reduces to **axis 2 plus a small maintainer tax** — and a correct
rewrite preserves axis 2 by construction.

### Faithfulness is *decidable* in the world we chose

Because Option 1 lives entirely in the regular world, "does the rewrite behave
identically?" is **machine-checkable**, not merely sampleable. Language equivalence
of two regexes is decidable; the stronger match-end-at-every-position equivalence a
lexer needs is also a regular property (the matched-prefix set from a position is
regular). The bundled rewrites are equivalent *precisely because they remove
ambiguity* (`"(?:[^"\\]|\\.)*"` is greedy, but `[^"\\]` cannot consume the closing
quote, so greedy and the original lazy `.*?` coincide). This guarantee is impossible
for a runtime engine that admits backreferences — equivalence there is undecidable.

### Severity asymmetry

Option 1's failure on an unsupported pattern is a **load-time, loud refusal**
surfaced to the grammar author. PR #110's faithfulness risk is a **parse-time,
silent divergence** surfaced (if ever) to a downstream consumer. Load-loud ≪
parse-silent. And PR #110's faithfulness on hand-written lookaround is *unverified*
(no differential fuzzer). **An honest rejection can be more faithful than a buggy
acceptance** — faithfulness is "never lies about the result," not "accepts more
inputs."

## 7. Tiering the grammar population

Every grammar lark-rs is handed falls into:

| Tier | What it is | Option 1 outcome | Sacrifice |
|---|---|---|---|
| T0 | No lookaround | combined DFA | none |
| T1 | Imports stdlib lookaround terminals | pre-rewritten bundled grammar | none (invisible) |
| T2 | Hand-written, reducible to regular | 1b auto-rewrites / 1a rejects | none (1b) or loud reject (1a) |
| T3 | Hand-written, bounded, **not** reducible in practice | loud reject | **the real sacrifice** |
| T4 | Backreferences / variable-width lookbehind | reject | none — Python `re` **and** PR #110 reject these too |

The entire sacrifice lives in **T3** (and T2 only under the lazy 1a variant). T0/T1
are free; T4 is a wash. So the empirical question is precisely: *how big is T3?*

## 8. Empirical evidence: a two-corpus census

We measured the base rate via GitHub code search across two corpora. **This is a
public-GitHub sample with the caveats in §9; it is not exhaustive.**

### Corpus A — `.lark` files

Query: `path:*.lark /\(\?[=!<]/` → **183 results**, of which the first page is
**51 unique (repo, file) pairs ≈ ~20 distinct grammars.** The count is heavily
**fork/vendor-inflated** (e.g. 5× `xlm-macro-en.lark`, 5× `pep508.lark`, 4×
`gdscript.lark`, 4× vendored `lark.lark`, 4× vendored `python.lark`).

### Corpus B — inline grammars in Python (`.lark`'s blind spot)

Query: `language:python /\(\?[=!<]/ "Lark("` → **145 results**, **38 on the first
page**. Noisier (matches lookaround anywhere in a file that also calls `Lark(`), but
it probes grammars defined as Python strings — the population `path:*.lark` misses.

### What both corpora contain

After de-forking, ~40 distinct grammars. **Every genuine case is bounded → regular
→ reducible.** The classes:

1. **The Python-string idiom** (`("(?!"").*?(?<!\\)(\\\\)*?"…)`), copy-pasted from
   `python.lark`: Vork, godot-gdscript-toolkit, mmlang, **erezsh/Preql** (Lark's own
   author), birp, **google-research/kauldron**, DianaVM, optimade, confit, spinta.
   This single idiom is the most common lookaround in the wild; its canonical
   lookaround-free form is `"(?:[^"\\]|\\.)*"`.
2. **The block-comment idiom** (`/\*(\*(?!\/)|[^*])*\*\//`): DianaVM,
   **microsoft/LayoutGeneration**.
3. **Reserved-word exclusion** (the Lark `unless`/keyword case):
   `chunkhound` (`IDENTIFIER: /(?!(END_VAR|END_PROGRAM|…))/`), **graphistry/pygraphistry**
   (`NAME: /(?!(?i:AND|OR|…))/`), `Hexa-Da/NanoC`.
4. **Operator / delimiter / boundary lookaheads:** `FUNCTION(?!_)`, `=(?!=|>)`,
   `:(?!:)`, `-(?!-)`, `(?=;|,)`, `(?![a-z])`, `(?![1-9])`, `/ +(?=[^.])/`,
   `(?!{{|…)`, `INT "." /(?![.])/`.
5. **Fixed-width lookbehind:** `pep508` `(?<====)` / `(?<===|!=)`, ROS
   `PACKAGE_NAME` `(?<!_)\/`, and the `(?<!\\)` of the string idiom.

### What both corpora do **not** contain

- **T3 (irreducible): zero.**
- **Backreferences: zero** (the `(?P<name>` hits are named *groups*, not backrefs —
  though note this query cannot find backrefs; see §9).
- **Variable-width lookbehind: zero** (every `(?<!` seen is the fixed-width `(?<!\\)`).
- **False positives confirmed:** `berlino/grammar-prompting` (`(?=` is literal DSL
  syntax inside quoted string terminals, an entire file of non-regex matches);
  `Bryantad/Sona` and `acorderob/...prompt-postprocessor` (lookaround in ordinary
  `re.sub` app code, not the grammar). So the true count is *below* the raw numbers.

### The one most-valuable find

**`strictdoc-project/strictdoc`** (a real, maintained docs-as-code tool) defines:

```
NODE_STRING_VALUE.2: /(?![ ]*##RELATION_MARKER_START)(?!…)…/
```

The `(?![ ]*##…)` is a negative lookahead with an **unbounded-width body** (`[ ]*`).
This is a concrete, in-the-wild instance of *exactly* the O(n²) hazard in PR #110's
Pike-VM (an assertion body containing `*`, re-evaluated per position). It is
simultaneously:

- the case PR #110's engine handles **worst** (super-linear), and
- **still regular**, hence handled **fine** by Option 1 (the body `[ ]*##…` is a
  regular language; the rewrite/automata path absorbs it).

So this single grammar argues both *for* removing the backtracking engine **and**
*against* shipping the unbounded-capable Pike-VM.

## 9. Coverage and limitations of the census

Stated honestly so the conclusion is not over-read:

- **Feature coverage:** the search finds only `(?=`/`(?!`/`(?<` openers. It does
  **not** search for backreferences (`\1`, `(?P=`, `\k<`), atomic groups (`(?>`),
  possessive quantifiers, or conditionals. "Zero backreferences" reflects what was
  *visible*, not a search for them. Suggested follow-ups: `path:*.lark /\\[1-9]/`,
  `path:*.lark /\(\?>/`, `path:*.lark /\(\?\(/`.
- **Corpus coverage:** public GitHub only; private/enterprise grammars are
  unmeasured. Corpus B is noisy and Corpus A misses inline grammars (mitigated by
  running both).
- **Sampling:** only the first results page of each query was classified by hand
  (51/183 and 38/145); the fork-heavy tails were extrapolated, not verified.
- **Why the decision survives the gaps anyway:** the unsearched features (T4) are
  rejected by Python `re` **and** by PR #110 alike, so they cannot favor either
  option — they only resize the loud-reject tail. And the theory dominates the
  census: bounded ⇒ reducible regardless of count. A wider search can change the
  *estimated size* of T3, not the architecture.

## 10. Recommendation

Adopt **Option 1b**:

1. **Ship lookaround-free equivalents of the bundled grammars** (`python.lark`,
   `lark.lark`, `common.lark`, and the `examples/` comment terminal). Prove each
   equivalent via the existing oracle matrix and, ideally, DFA match-length
   equivalence. These rejoin the combined-DFA scanner.
2. **Add a small load-time rewrite pass** for the two dominant reducible classes:
   the Python-string/block-comment idioms, and boundary / reserved-word / fixed-width
   assertions. This auto-handles essentially the entire observed wild population.
3. **Reject anything genuinely irreducible** (the empty-in-practice T3, plus T4) with
   a **loud, actionable build-time error** that names the terminal and suggests the
   rewrite or `unless`/rules.
4. **Retire the runtime lookaround engine** (or keep a tiny, *bounded*, fuzzed
   Pike-VM strictly as a rarely-hit fallback, never on the common path).

This keeps the single fast linear engine, removes the ReDoS, removes the
parity-with-`re` maintenance surface, and preserves observable behavior for every
grammar shown to work in Python Lark today.

### What PR #110 still contributes

Removing `fancy-regex` and the deterministic linearity gate are good and reusable
regardless. If verbatim upstream text is later deemed non-negotiable (axis 3
elevated above all), the fallback is "Option A, fixed": delegate whole spans to
`regex` (recover speed), enforce assertion boundedness (close the `strictdoc`-shaped
O(n²) hole), and add a CPython differential fuzzer (bound the parity surface).

## Appendix: distinct grammars observed (de-forked)

Corpus A (`.lark`):

| Project (representative) | File | Lookaround | Class |
|---|---|---|---|
| ytsaurus (vendored) | `lark.lark` | `OP`, `REGEXP` | stdlib (T1) |
| poetry-core (vendored) | `python.lark` | `STRING`/`LONG_STRING`/`DEC_NUMBER` | stdlib (T1) |
| godot-gdscript-toolkit | `gdscript.lark` | string idiom | T2-reducible |
| DissectMalware/XLMMacroDeobfuscator | `xlm-macro-en.lark` | `NAME /…(?!\d{1,6}\b)…/` | T2-reducible |
| poetry/conda pep508 | `pep508.lark` | `(?<====)`, `(?<===\|!=)` | T2 (fixed-width behind) |
| Systems-Modeling/SysML | `kgbnf…lark` | `(?![ \t])` | T2-reducible |
| chunkhound | `twincat/declarations.lark` | `FUNCTION(?!_)`, reserved-word `IDENTIFIER` | T2 (`unless`) |
| microsoft/LayoutGeneration | `grammar_rico.lark` | comment idiom + escaped id | T2-reducible |
| google-research/kauldron | `path_grammar.lark` | string idiom | T2-reducible |
| vertexproject/synapse | `imap.lark` | `(?!\r\|\n\|\\"\|\\\\).` | T2 (negated set) |
| Itay2805/Vork | `v.lark` | string idiom | T2-reducible |
| Extelligence-ai/bagel | `ros1/grammar.lark` | `(?<!_)\/` | T2 (fixed-width behind) |
| amplify-education/python-hcl2 | `hcl2.lark` | `=(?!=\|>)`, `:(?!:)`, `STRING_CHARS` | T2-reducible |
| erezsh/Preql | `preql.lark` | string idiom, `INT "." /(?![.])/` | T2-reducible |
| evtn/birp | `birp.lark` | string idiom | T2-reducible |
| hpc/pavilion2 | `filters.lark` | `/ +(?=[^.])/` | T2-reducible |
| thautwarm/DianaVM | `ch.lark` | string + comment idiom | T2-reducible |
| Materials-Consortia/optimade | `v1.2.0.lark` | `(?<!\\)(\\\\)*?` | T2-reducible |
| colun/mmlang | `mmlang.lark` | string idiom | T2-reducible |
| berlino/grammar-prompting | `lispress_full_3.lark` | — | **false positive** |

Corpus B (inline Python):

| Project (representative) | File | Lookaround | Class |
|---|---|---|---|
| lark-parser/lark-language-server | `lark_grammar.py` | `OP: /[+*]\|[?](?![a-z])/` | stdlib (T1) |
| graphistry/pygraphistry | `expr_parser.py` | `-(?!-)`, reserved-word `NAME` | T2 (`unless`) |
| theY4Kman/parsuricata (+pCraft) | `_parser.py` | `(?=;\|,)`, `LITERAL` | T2-reducible |
| vertexproject/synapse | `imap.py` | `.*?(?! {…)` | T2-reducible |
| strictdoc-project/strictdoc | `marker_lexer.py` | `(?![ ]*##RELATION_MARKER_START)` | T2-reducible **(unbounded body — PR #110 O(n²))** |
| nlothian/Vibe-Prolog | `parser.py` | `-?(?=[…])`, `(?![a-zA-Z0-9_])` | T2-reducible |
| hpc/pavilion2 | `strings.py` | `(?!{{\|…)`, `(?=$\|}}\|{{…)` | T2-reducible |
| Hexa-Da/NanoC | `nanoC.py` | reserved-word `IDENTIFIER` | T2 (`unless`) |
| aphp/confit | `xjson.py` | string idiom | T2-reducible |
| atviriduomenys/spinta | `spyna.py` | string idiom | T2-reducible |
| hyphatech/jailrun | `ucl.py` | `(?=[…` | T2-reducible |
| luan-xiaokun/isabelle-export-deps | `root_parser.py` | `(?:(?!…)` | T2-reducible |
| Bryantad/Sona | `lsp_server.py` | `re.sub(r'(?<!=)=(?!=)'…)` | **false positive** |
| acorderob/sd-webui-prompt-postprocessor | `ppp.py` | `re` app code | **false positive** |
| IfcOpenShell, penn-courses, tracardi | (various) | not captured | unclassified |
