# Lookaround in lark-rs: decision memo

*Status: **superseded in part** — see the revision below. Original decision: pure
elimination (Option 1b). Active plan is now
[`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md).*
*Date: 2026-06-07.*

> ## Revision (2026-06-08) — a DFA lexer engine *is* warranted, for speed + bakeability
>
> This memo concluded "ship **no** runtime engine" (Option 1b). That conclusion was
> right **for correctness** and stands: a bounded assertion is regular, so no engine is
> needed to *parse* correctly. It is now **reversed on a different axis** — throughput
> and bakeability — after the
> [terminal-reduction diagnosis](TERMINAL_REDUCTION_DIAGNOSIS.md) and the architecture
> analysis that followed it. The reasoning:
>
> * Elimination alone leaves the **G-tier** terminals (`STRING`, `OP`, `DEC_NUMBER` —
>   provably not rewritable to a plain `regex` string) on the slow `fancy-regex`
>   per-terminal side-probe **forever**. To give them single-pass speed *and* make the
>   `python`/`lark` grammars bakeable, they must join one combined automaton.
> * Their assertions are **bounded** ⇒ regular ⇒ **lowerable** into ordinary DFA
>   states. So the engine that includes them is a **DFA** (built on `regex-automata`),
>   **not** the runtime Pike-VM this memo rejected.
> * **The anti-engine arguments here do not apply to a DFA.** §3's triangle and §4's
>   "don't maintain a CPython-`re`-parity automaton" target an engine that *executes*
>   lookaround at match time. A DFA over **lowered, lookaround-free** terminals executes
>   no lookaround and has **no parity surface** — it is a plain regular automaton,
>   machine-checkable against the `regex` crate (the L0 differential oracle). And a DFA
>   is *faster* than the Pike-VM (one lookup/byte), so §4's speed objection also
>   inverts.
>
> Net: the elimination work (below) is retained as **Phase 1** of `LEXER_DFA_PLAN.md`;
> the new phases build the combined DFA, lower the G-tier into it, and bake it. What is
> *not* revived is PR #110's Pike-VM `matcher.rs`. The memo's analysis below remains the
> correctness record; read it together with this revision.

> **Outcome (original, 2026-06-07).** This memo evaluated [PR #110](https://github.com/okalldal/lark/pull/110)
> ("Lexer DFA M1–M3 — lower lookaround to a linear Pike-VM engine, remove
> fancy-regex"). The conclusion was to **not** ship the runtime Pike-VM and instead
> eliminate lookaround at grammar-load time. **PR #110 was closed** (not merged); its
> branch is preserved as the spec + working implementation of the shelved Option-H
> fallback engine, to revisit only if a real irreducible-but-valid bounded lookaround
> grammar ever appears. The former `LEXER_DFA_PLAN.md` (the lowering strategy) was
> removed and superseded by `LOOKAROUND_ELIMINATION_PLAN.md`; it remains in git history
> and on the closed-PR branch. *(Revived 2026-06-08 — see revision above.)*

## TL;DR

PR #110 removes the `fancy-regex` backtracking overlay (a real ReDoS) and replaces
it with a hand-rolled linear **Pike-VM** that *executes* bounded lookaround at match
time. This memo asks whether that is the right shape and concludes:

1. **An elimination fast-path should exist regardless.** The bundled terminals and
   the common reducible idioms can be rewritten to lookaround-free regex at
   grammar-load time, rejoin the combined-DFA scan, and run faster with provably
   identical behavior. This is pure win and both approaches should include it.
2. **Full, *general* elimination is not a small pass.** A bounded assertion is
   regular as a *recognizer*, but a lexer needs Python's leftmost-first/lazy
   **priority** match semantics, and there is no known way to rewrite an arbitrary
   bounded assertion to a priority-preserving lookaround-free *regex string*. General
   priority-preserving elimination yields an *automaton with priority* — which is
   exactly a Pike-VM. So "auto-rewrite the reducible class" is realistically a
   **finite template set**, not a general algorithm. (This corrects an overclaim in
   the first draft of this doc, raised by the PR author — see §6.)
3. **Therefore the real decision is only about the long tail:** a *novel but valid
   bounded* lookaround that no template matches. The census (§10) finds **zero** such
   patterns across ~40 distinct real grammars, so the tail is empirically empty.

**Recommendation: pure elimination, no runtime engine ("Option 1b").** Rewrite the
bundled grammars and the common reducible idioms to lookaround-free regex so they run
on the `regex` crate (rejoining the fast combined-DFA scan), and **reject** any
remaining lookaround with a loud, actionable build-time error. **Do not ship the
Pike-VM.** The rationale: maintaining a hand-rolled engine that matches CPython `re`
byte-for-byte is a large, brittle, permanent cost (axis 4), and the census shows it
would serve a population that does not exist. A reject is a humane, *visible* failure
with a fix-it message — and a signal: if real "valid in Python Lark, rejected here"
reports ever appear, add the bounded fallback **then** (YAGNI). Building the engine
speculatively is the expensive mistake; deferring it is cheap and reversible.

**Alternative (Option H) — only if "never reject a bounded-lookaround grammar that
Python Lark accepts" is a hard, non-negotiable requirement:** keep PR #110's engine
as a bounded, fuzzed, off-the-hot-path fallback behind the elimination fast-path.
This buys full tail compatibility at the price of carrying the parity surface
forever — a premium the evidence says insures against nothing today, which is why it
is the fallback choice, not the default.

Either way, the elimination fast-path is **mandatory** — it is the part that wins
speed and faithfulness on every grammar that exists. The only open question is what
to do with the (empirically empty) tail: reject (recommended) or fall back to an
engine.

---

## 1. Context and problem

The `regex` crate (and `regex-automata`) reject lookaround and backreferences
because they break the linear-time finite-automaton model. A few of Lark's bundled
grammars use **bounded** lookaround:

| Terminal | Grammar | Assertion(s) |
|---|---|---|
| `STRING`, `LONG_STRING` | `python.lark` | `(?!"")`/`(?!'')`, `(?<!\\)(\\\\)*?` |
| `REGEXP`, `OP` | `lark.lark` | `(?!\/)`, `(?![a-z])` |
| `DEC_NUMBER` | `common.lark` | `(?![1-9])` |
| `MULTILINE_COMMENT` | `verilog.lark` (examples) | `\*(?!\/)` |

lark-rs previously routed these to `fancy-regex` (backtracking), which carried a real
ReDoS in `lark.REGEXP` and a bail-to-wrong-answer risk on backtrack-limit. PR #110
replaces that with a linear Pike-VM and deletes `fancy-regex`.

## 2. Decision drivers (the axes)

1. **Linear / no ReDoS** (safety)
2. **Faithful Python-`re` behavior** (correctness as oracle)
3. **Verbatim upstream grammar text** (drop-in compatibility)
4. **Small maintenance surface**
5. **Average-case speed**
6. **WASM / C / standalone bakeability**

`fancy-regex` gives 2,3,5,6 but not 1,4. PR #110's Pike-VM gives 1,2,3,6 but
sacrifices 5 and (because it is a bespoke parity surface) 4.

## 3. The core constraint: an impossibility triangle

You cannot have all three of:

- **(C) Compatibility** — accept every bounded lookaround Python Lark accepts.
- **(L) Hard linearity** — a guaranteed linear-time bound on every input.
- **(S) No bespoke priority engine** — match purely on the `regex` crate, no
  hand-rolled priority automaton to maintain.

- **PR #110** takes **C + L**, so it must pay **S** (the Pike-VM). And it currently
  leaks **L** on unbounded-width lookahead bodies (see §7), so in practice it is
  C + *soft*-L + ¬S.
- **Pure elimination ("Option 1b")** takes **L + S**, so it must pay **C** (reject
  the novel-pattern tail). The census (§5) shows that tail is empirically tiny, but
  it is not empty in principle.
- **`fancy-regex`** took **C** alone (¬L, and ¬S since it is still a dependency).

Naming this triangle is the point of the memo: there is no option that is
simultaneously fully compatible, hard-linear, and engine-free. Every choice below is
a position on it.

## 4. Options

- **Option 0 — keep `fancy-regex`.** Rejected: ReDoS + bail-to-wrong-answer. (The
  "can't bake into WASM/C" argument is weak — `fancy-regex` is pure Rust; the honest
  sole justification is ReDoS.)
- **Option A — PR #110's Pike-VM** as the primary lookaround path. C + L − S, with
  the §7 linearity leak.
- **Option 1a — manual bundled rewrites + reject all other lookaround.** L + S − C,
  maximal compatibility cost.
- **Option 1b (recommended) — manual bundled rewrites + a finite auto-rewrite
  template set + reject the rest.** L + S − C, smaller compatibility cost than 1a but
  still real in principle (see §6) — though the census (§10) shows the rejected set is
  empty in practice. **No runtime engine.**
- **Option H (hybrid, fallback choice) — elimination fast-path + bounded fallback VM.**
  Reducible idioms are rewritten and rejoin the combined-DFA scan (speed); the
  remaining bounded lookaround runs on PR #110's engine as a rare, bounded, fuzzed
  fallback (compatibility). C + L − (partial S): keeps the engine, but off the hot
  path and with boundedness enforced. Only warranted if zero-rejection is a hard
  requirement; otherwise it pays the parity-surface premium to insure against an
  empty population.

### Why not "just compile to a DFA"

A DFA gives longest-match / recognition. Python's `STRING` uses **lazy** `.*?`
(shortest) with an assertion selecting the closing quote; a longest-match DFA would
swallow to the last quote in the file. So priority semantics are mandatory, and a
Pike-VM is the right linear engine for them. **PR #110's core engine choice is
sound.** The disagreement is only about whether it sits on the hot path.

## 5. Speed: why the elimination fast-path matters (axes 1, 5)

Both engines are linear, so axis 1 is a tie. Axis 5 favors elimination, and the gap
is **structural**, at two levels:

- **Micro:** a rewritten lookaround-free pattern is one `regex` automaton — a
  table-driven, prefiltered, byte-at-a-time pass. The Pike-VM pays thread-list /
  ε-closure bookkeeping and re-entry at each assertion seam.
- **Macro (decisive):** a lookaround terminal **cannot join the combined scanner
  alternation** (the combined `regex` can't express assertions), so it is probed
  individually, anchored, at every token boundary. A rewritten terminal rejoins
  `(?P<g>…)|…` and rides the single combined-DFA pass. **A DFA matches N alternatives
  in one pass; the side-probe matches them in N passes.** With `k` lookaround
  terminals over `T` tokens, the side-probe adds ~`O(k·T)` work the combined scan
  gets for free.

This is why the fast-path is worth having *regardless of the tail decision*: it moves
the bundled terminals and common idioms — the bulk of real lookaround (§5 census) —
onto the fast path, leaving only the rare tail on the slower per-terminal probe.

## 6. The generality limitation of "auto-rewrite" (the correction)

The first draft framed Option 1b's auto-rewrite as if it were near-general. It is
not, and the PR author's objection is correct. The precise statement:

- A bounded assertion denotes a regular **language**, so an equivalent
  lookaround-free **recognizer** always exists.
- But a lexer needs the **match-end function under leftmost-first/greedy-lazy
  priority**, not language membership. Two regexes with the same language can have
  different match-end functions.
- The clean bundled rewrites (e.g. `"(?!"").*?(?<!\\)(\\\\)*?"` → `"(?:[^"\\]|\\.)*"`)
  work because they **remove ambiguity** — once `[^"\\]` cannot consume the closing
  quote, greedy and the original lazy `.*?` coincide. That is a **per-idiom
  insight**, not a general procedure.
- General, priority-preserving elimination of an arbitrary bounded assertion yields
  an **automaton with priority**, which is exactly a Pike-VM. There is no known
  algorithm that emits a priority-preserving lookaround-free **regex string** for the
  general case.

**Consequence — and the cost the first draft underweighted:** a realistic
auto-rewriter is a **finite template set** (escaped-string family, block-comment,
negated-char lookahead `(?!set). → [^set]`, reserved-word exclusion `(?!(KW|…)) →`
`unless`/keyword priority, fixed-width lookbehind, leading/trailing boundary
assertions). The reject-class is therefore **"not template-matched," not
"irreducible."** That gap is real: a user could write a *novel but perfectly valid
bounded* lookaround that Python Lark accepts and that no template covers, and pure
1a/1b would reject it. The census says this is empirically near-empty, but
"empirically empty in a public sample" ≠ "impossible," and rejecting valid input is a
genuine compatibility regression versus Python Lark.

This gap is what **Option H** would close by keeping PR #110's engine as a fallback:
the engine *is* the general priority automaton, so the novel tail would keep working
instead of being rejected. But note how narrow that tail is — some classes *do* admit
general sub-algorithms (boundary assertions, negated-char lookahead, and reserved-word
exclusion are general, not template-bound), so a fallback would only ever see
genuinely internal, length-changing, novel assertions, which the census finds to be
**none**. That is why the recommendation is to **reject** this tail (pure 1b) rather
than carry an engine for it: you would be maintaining a CPython-`re`-parity automaton
to serve the empty set.

### The one tension neither option dissolves

Unbounded-width lookahead bodies (e.g. `(?![ ]*X)`) are accepted by Python `re`
(itself backtracking, hence potentially non-linear there too), but a hard linear
bound requires either rejecting them or accepting super-linear cost. So *every* option
that keeps linearity (including Option H, were it adopted) must choose, for that
sub-case, between **C** (accept, match Python, risk O(n²)) and **L** (reject/limit).
The memo picks **L** — reject unbounded lookahead with a clear message — because a
guaranteed bound is the whole reason for leaving `fancy-regex`. This is a small,
well-defined
slice of **C** to give up, and `strictdoc` (§7) is the only observed instance.

## 7. The unbounded-lookahead hazard (a standalone review note)

PR #110's engine width-checks **lookbehind** but not **lookahead** bodies, so
`(?![ ]*X)` inside a quantifier is re-evaluated per position → **O(n²)**. This is
reachable by real grammars: `strictdoc-project/strictdoc` defines
`NODE_STRING_VALUE.2: /(?![ ]*##RELATION_MARKER_START)(?!…)…/`. It is simultaneously
the case PR #110 handles **worst** (super-linear) and a case still **regular** (so
the fast-path/fallback can handle it correctly; the choice is only whether to accept
the super-linear cost or reject for linearity — see §6). Recommendation: enforce
assertion boundedness, or document the guarantee as "linear for bounded assertions."

## 8. Faithfulness, decomposed (axes 2, 3)

- **Axis 2 = behavior; Axis 3 = artifact (byte-identical `.lark`).**
- **Axis 3 largely collapses into axis 2 and is mostly invisible:** `%import
  python.STRING` loads *lark-rs's* bundled grammar; the user never reads it, so an
  internal rewrite that is behavior-identical is unobservable. Axis 3's residue is a
  maintainer-side upstream-sync tax plus the rare user who pastes a whole upstream
  grammar as their own source.
- **Faithfulness is decidable in the regular world:** equivalence of the bundled
  rewrites is machine-checkable (language equivalence, and the stronger
  match-end-at-every-position equivalence, are regular properties). This guarantee is
  impossible once backreferences are admitted.
- **Severity asymmetry:** an elimination reject is a **load-time, loud** error to the
  grammar author; an engine parity bug is a **parse-time, silent** divergence to a
  downstream consumer. Load-loud ≪ parse-silent. The reject does deny a valid grammar
  (§6) — but it denies it *visibly, at build time, with a fix-it message*, which is a
  far better failure than a silent mis-parse from an under-tested engine. This is part
  of why pure 1b (reject the empty tail) is preferred over carrying an engine to avoid
  the reject.

## 9. Tiering the grammar population

| Tier | What it is | Fast-path | Fallback (Option H) | Pure 1b |
|---|---|---|---|---|
| T0 | No lookaround | combined DFA | — | combined DFA |
| T1 | Imports stdlib lookaround | pre-rewritten, rejoins DFA | — | same |
| T2 | Hand-written, template-matched | rewritten, rejoins DFA | — | rewritten |
| T2′ | Hand-written, reducible but **novel** (no template) | — | bounded VM ✅ | **reject ❌** |
| T3 | Bounded, internal, length-changing, novel | — | bounded VM ✅ | **reject ❌** |
| T4 | Backref / variable-width behind / unbounded-ahead | reject | reject (boundedness) | reject |

The first draft hid **T2′** inside "reducible," implying the auto-rewriter covered
it. It does not. T2′ + T3 are the compatibility cost of pure 1a/1b — a **loud reject**
of the few patterns no template covers. The census (§10) finds T2′ + T3 empty, so the
recommended choice is to accept that (empty) cost and reject, rather than carry the
fallback engine (Option H) to avoid it. T4 is rejected by Python `re` too (except
unbounded-ahead; see §6/§7).

## 10. Evidence: a two-corpus census

Public-GitHub sample; caveats in §11. Measures the size of the at-risk tail
(T2′/T3/T4).

**Corpus A — `.lark` files.** `path:*.lark /\(\?[=!<]/` → **183 results**, first page
**51 unique pairs ≈ ~20 distinct grammars**, heavily fork/vendor-inflated.

**Corpus B — inline Python grammars.** `language:python /\(\?[=!<]/ "Lark("` →
**145 results**, **38 on the first page**. Noisier, but covers the population
`path:*.lark` misses.

**After de-forking (~40 distinct grammars), every genuine case is bounded and falls
into a small set of classes:**

1. **Python-string idiom** (most common; canonical rewrite `"(?:[^"\\]|\\.)*"`):
   Vork, godot-gdscript-toolkit, mmlang, **erezsh/Preql**, birp,
   **google-research/kauldron**, DianaVM, optimade, confit, spinta.
2. **Block-comment idiom** (`/\*(\*(?!\/)|[^*])*\*\//`): DianaVM,
   **microsoft/LayoutGeneration**.
3. **Reserved-word exclusion** (the `unless` case): chunkhound, **pygraphistry**,
   NanoC.
4. **Operator/delimiter/boundary lookahead:** `FUNCTION(?!_)`, `=(?!=|>)`, `:(?!:)`,
   `-(?!-)`, `(?=;|,)`, `(?![a-z])`, `(?![1-9])`, `/ +(?=[^.])/`, `(?!{{|…)`.
5. **Fixed-width lookbehind:** pep508 `(?<====)`/`(?<===|!=)`, ROS `(?<!_)\/`, and the
   string idiom's `(?<!\\)`.

**Not found:** irreducible (T3) cases — **zero**; backreferences — **zero** (the
`(?P<name>` hits are named *groups*; note this query cannot find backrefs, §11);
variable-width lookbehind — **zero**. **One** unbounded-lookahead-body (strictdoc,
§7). **False positives confirmed:** berlino/grammar-prompting (literal `(?=` in DSL
string terminals), Bryantad/Sona and acorderob/…prompt-postprocessor (`re.sub` in app
code) — so true counts are below the raw numbers.

So the at-risk tail (T2′/T3) is, in this sample, **empty**; everything is T1, a
template class, or a false positive. That makes pure 1b's compatibility cost zero in
practice. §6 notes "empty in a sample" is not "impossible in principle" — but the
right response to an empty population is to **reject it loudly and skip the engine**,
not to build and maintain a CPython-`re`-parity automaton against the day someone
might need it. If that day comes, the loud reject reports it and Option H is a clean
additive follow-up.

## 11. Coverage and limitations

- **Feature coverage:** the search finds only `(?=`/`(?!`/`(?<`. It does **not**
  search backreferences (`\1`, `(?P=`, `\k<`), atomic groups (`(?>`), possessive
  quantifiers, or conditionals. "Zero backreferences" = not visible, not searched.
  Follow-ups: `path:*.lark /\\[1-9]/`, `/\(\?>/`, `/\(\?\(/`.
- **Corpus coverage:** public GitHub only; private grammars unmeasured. Corpus B is
  noisy; Corpus A misses inline grammars (mitigated by running both).
- **Sampling:** only the first results page of each query was hand-classified
  (51/183, 38/145); fork-heavy tails extrapolated.
- **Why the decision survives the gaps:** the unsearched features are T4 — rejected
  by Python `re` and by every option here — so they cannot favor one option; they
  only resize the reject tail. And the theory (§3, §6) dominates the census.

## 12. Reasoning chain (how the conclusion follows)

The logical spine, tying the **axes** (§2) and **usage tiers** (§9) to the
recommendation:

1. **Frame by axes (§2).** No engine maxes all six axes at once. `fancy-regex` fails
   linearity (1) and maintenance surface (4); PR #110's Pike-VM fails average-case
   speed (5) and is itself a parity surface (4). The conflict is real, so a choice is
   unavoidable.
2. **The conflict is structural → the impossibility triangle (§3).** Compatibility
   (C), hard linearity (L), and no-bespoke-engine (S) cannot all hold. Every option is
   a position on this triangle, which is what makes the decision principled rather
   than a matter of taste.
3. **The DFA shortcut is closed (§4, §6).** Python's lazy `.*?` + assertion needs
   *priority* match semantics, so longest-match automata cannot stand in. The only
   general linear matcher for priority semantics is a Pike-VM. Therefore **S is
   attainable only by surrendering some C**: you can template the common reducible
   shapes, but there is no general priority-preserving rewrite to a plain regex
   string.
4. **Speed splits along the combined-scan boundary (§5).** Anything expressible
   *without* assertions rejoins the single-pass combined DFA; anything *with*
   assertions is an N-pass per-terminal side-probe. So eliminating the reducible
   cases is a structural speed win **regardless of the tail decision**.
5. **Faithfulness decomposes (§8).** Axis 3 (verbatim text) mostly collapses into
   axis 2 (behavior) for importers, and behavior-equivalence of the *regular*
   rewrites is machine-checkable. So the elimination fast-path costs ~nothing on
   faithfulness for the cases it covers.
6. **The tiers (§9) localize the only real cost.** T0/T1/T2 are handled by the
   fast-path with no compatibility loss; T4 is rejected by every option (Python `re`
   included). The entire disagreement is **T2′/T3** — novel bounded lookaround with no
   template.
7. **The census (§10) sizes that tail.** Across ~40 distinct real grammars,
   T2′/T3 = empty; the population is fork-inflated idioms. So the disputed cost is
   empirically tiny — but §6 shows it is not zero *in principle*.
8. **Conclusion.** Do the fast-path regardless (free speed + faithfulness on T0–T2).
   For the T2′/T3 tail, the census shows it is empirically empty — so **reject it with
   a loud, actionable error and do not ship the engine** (Option 1b). Carrying a
   CPython-`re`-parity engine to serve a population that does not exist is the
   expensive mistake; a loud reject is humane and self-reporting, and the bounded
   fallback (Option H) can be added later *if* a real case ever appears. YAGNI.

## 13. Recommendation and consequences

**Recommend pure elimination (Option 1b) — no runtime engine:**

1. **Elimination fast-path (the whole solution).** Rewrite the bundled grammars and
   the general/template-able classes (boundary assertions, negated-char lookahead,
   reserved-word exclusion → `unless`, fixed-width lookbehind, the string/comment
   idioms) to lookaround-free form so they rejoin the combined-DFA scan. Verify
   equivalence via the existing oracle matrix and, ideally, DFA match-length
   equivalence. **Wins: speed (axis 5), no parity surface (axis 4), linearity (axis
   1), behavior parity on every grammar that exists (axis 2).**
2. **Loud reject for everything else.** Any lookaround the fast-path can't lower
   (the empirically empty T2′/T3, plus all of T4 — backref / variable-width behind /
   unbounded-ahead) is a clear, actionable build-time error naming the terminal and
   suggesting the fix (rewrite as X, use a rule, or import the stdlib terminal).
   **Do not ship the Pike-VM.**

**What we explicitly give up:** any *novel, non-template, bounded* lookaround a user
might write is rejected even though Python Lark accepts it (§6). The census says this
set is empty today; the cost is a *potential future* "works in Python Lark, rejected
here" report — which arrives as a loud build error, not a silent mis-parse, and tells
us exactly when (if ever) to revisit. Unbounded-width lookahead (`strictdoc`'s
`(?![ ]*X)`) is also rejected, trading that slice of compatibility for the hard linear
guarantee that motivated leaving `fancy-regex`.

**Alternative — Option H, only if zero-rejection is a hard requirement.** Keep
PR #110's engine as a bounded, fuzzed, off-the-hot-path fallback behind the
fast-path: route the non-template bounded tail to it, enforce assertion boundedness
(close the §7 hole), add a CPython differential fuzzer. This eliminates the reject at
the cost of carrying the parity surface forever. Not recommended, because the evidence
says it insures against nothing — but it is a clean, additive next step if a real
T2′/T3 case ever materializes.

**What PR #110 contributes regardless:** removing `fancy-regex` and the deterministic
linearity gate are keepers. The Pike-VM itself is not thrown away so much as *shelved*
— if Option H is ever needed, the engine already exists and just needs the
boundedness check + fuzzer before being wired in as the fallback.

## 14. Open questions / follow-ups

- Prove the bundled rewrites behavior-identical through the oracle matrix (and DFA
  match-length equivalence) — turns the §5 idiom claims into facts.
- Run the §11 backref/atomic/conditional queries and paginate the censuses to firm up
  the T4 estimate.
- Decide the unbounded-lookahead policy (§6/§7): reject for **L**, or accept for
  **C** with a documented non-linear caveat.
- Decide axis-4 weight: loud-reject (pure 1b, recommended) vs. fallback engine
  (Option H, only if zero-rejection becomes a hard requirement). The recommended
  default is reject; revisit only if a real T2′/T3 report appears.

## Appendix: distinct grammars observed (de-forked)

Corpus A (`.lark`):

| Project (representative) | File | Lookaround | Class |
|---|---|---|---|
| ytsaurus (vendored) | `lark.lark` | `OP`, `REGEXP` | stdlib (T1) |
| poetry-core (vendored) | `python.lark` | `STRING`/`LONG_STRING`/`DEC_NUMBER` | stdlib (T1) |
| godot-gdscript-toolkit | `gdscript.lark` | string idiom | template |
| DissectMalware/XLMMacroDeobfuscator | `xlm-macro-en.lark` | `NAME /…(?!\d{1,6}\b)…/` | template/boundary |
| poetry/conda pep508 | `pep508.lark` | `(?<====)`, `(?<===\|!=)` | fixed-width behind |
| Systems-Modeling/SysML | `kgbnf…lark` | `(?![ \t])` | boundary |
| chunkhound | `twincat/declarations.lark` | `FUNCTION(?!_)`, reserved-word `IDENTIFIER` | `unless`/boundary |
| microsoft/LayoutGeneration | `grammar_rico.lark` | comment idiom + escaped id | template |
| google-research/kauldron | `path_grammar.lark` | string idiom | template |
| vertexproject/synapse | `imap.lark` | `(?!\r\|\n\|\\"\|\\\\).` | negated set |
| Itay2805/Vork | `v.lark` | string idiom | template |
| Extelligence-ai/bagel | `ros1/grammar.lark` | `(?<!_)\/` | fixed-width behind |
| amplify-education/python-hcl2 | `hcl2.lark` | `=(?!=\|>)`, `:(?!:)`, `STRING_CHARS` | boundary/internal |
| erezsh/Preql | `preql.lark` | string idiom, `INT "." /(?![.])/` | template/boundary |
| evtn/birp | `birp.lark` | string idiom | template |
| hpc/pavilion2 | `filters.lark` | `/ +(?=[^.])/` | boundary |
| thautwarm/DianaVM | `ch.lark` | string + comment idiom | template |
| Materials-Consortia/optimade | `v1.2.0.lark` | `(?<!\\)(\\\\)*?` | template |
| colun/mmlang | `mmlang.lark` | string idiom | template |
| berlino/grammar-prompting | `lispress_full_3.lark` | — | **false positive** |

Corpus B (inline Python):

| Project (representative) | File | Lookaround | Class |
|---|---|---|---|
| lark-parser/lark-language-server | `lark_grammar.py` | `OP: /[+*]\|[?](?![a-z])/` | stdlib (T1) |
| graphistry/pygraphistry | `expr_parser.py` | `-(?!-)`, reserved-word `NAME` | `unless`/boundary |
| theY4Kman/parsuricata (+pCraft) | `_parser.py` | `(?=;\|,)`, `LITERAL` | boundary |
| vertexproject/synapse | `imap.py` | `.*?(?! {…)` | boundary |
| strictdoc-project/strictdoc | `marker_lexer.py` | `(?![ ]*##RELATION_MARKER_START)` | **unbounded body — see §7** |
| nlothian/Vibe-Prolog | `parser.py` | `-?(?=[…])`, `(?![a-zA-Z0-9_])` | boundary |
| hpc/pavilion2 | `strings.py` | `(?!{{\|…)`, `(?=$\|}}\|{{…)` | delimiter/boundary |
| Hexa-Da/NanoC | `nanoC.py` | reserved-word `IDENTIFIER` | `unless` |
| aphp/confit | `xjson.py` | string idiom | template |
| atviriduomenys/spinta | `spyna.py` | string idiom | template |
| hyphatech/jailrun | `ucl.py` | `(?=[…` | boundary |
| luan-xiaokun/isabelle-export-deps | `root_parser.py` | `(?:(?!…)` | boundary |
| Bryantad/Sona | `lsp_server.py` | `re.sub(r'(?<!=)=(?!=)'…)` | **false positive** |
| acorderob/sd-webui-prompt-postprocessor | `ppp.py` | `re` app code | **false positive** |
| IfcOpenShell, penn-courses, tracardi | (various) | not captured | unclassified |
