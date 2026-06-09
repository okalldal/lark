# Lexer DFA plan — one combined automaton over all terminals

*Status: active umbrella plan for the lexer's lookaround/throughput work.*
*Supersedes the framing of [`LOOKAROUND_ELIMINATION_PLAN.md`](LOOKAROUND_ELIMINATION_PLAN.md),
which becomes **Phase 1** here. Rationale and the decision reversal are recorded in
[`LOOKAROUND_STRATEGY_ANALYSIS.md`](LOOKAROUND_STRATEGY_ANALYSIS.md) (revision
2026-06-08). Terminal-level classification in
[`TERMINAL_REDUCTION_DIAGNOSIS.md`](TERMINAL_REDUCTION_DIAGNOSIS.md).*
*Date: 2026-06-08.*

## Goal

Lex **every** terminal — lookaround-bearing ones included — in a **single
table-driven pass** over one combined automaton, built once and bakeable as static
data. Concretely: build the combined scanner on `regex-automata` (lazy/dense DFA,
multi-pattern `PatternID`), **lower** each bounded lookaround assertion into
lookaround-free automaton states so it joins the same machine, drive it with a
maximal-munch loop that reproduces Lark's exact selection, and drop `fancy-regex` from
the runtime.

Wins, in priority order:

1. **Throughput.** Today the lookaround terminals are *N separate `fancy-regex`
   side-probes per position* (`Scanner::match_at`); folding them into the one combined
   DFA removes the per-terminal engine entry — one array lookup per byte for the whole
   terminal set.
2. **Bakeability.** A serialized `regex-automata` DFA is static data, so the bundled
   `python`/`lark` (lookaround) grammars finally bake into the standalone / C / WASM
   runtimes — closing the standing limitation that those grammars are not
   standalone-able.
3. **Linearity / no ReDoS** and **removing the runtime `fancy-regex` dependency**, as a
   consequence of (1).
4. **A general feature, not a six-terminal patch** — see "What we support."

## This is a DFA, not the Pike VM of PR #110

The closed [PR #110](https://github.com/okalldal/lark/pull/110) shipped a runtime
**Pike-VM** that *executes* lookaround at match time, and the strategy memo rejected it
(maintenance/parity surface, slower than a DFA). **This plan does not revive that.**
The engine here is a **DFA** over terminals whose bounded assertions have been *lowered
away* — so:

* there is **no runtime lookaround execution** and **no CPython-`re`-parity surface**:
  the lowered terminals are ordinary regular languages, machine-checkable against
  `fancy-regex` (see Verification);
* a DFA is the *fastest* engine for this (one lookup/byte), where the Pike VM is
  linear-but-slower; PR #110's engine was suboptimal on both correctness-surface *and*
  speed.

The salvage from PR #110 is its **lookaround front-end** (`src/lookaround/`, the
assertion parser/classifier) — repurposed as the **lowering** pass that feeds the NFA
builder — **not** its `matcher.rs` Pike-VM.

## Why now (the reversal)

The elimination plan (Phase 1) gets the **Tier-E** terminals — the reducible bulk
(string/comment idioms) — back onto the combined `regex` DFA. But the **G-tier**
terminals (`STRING`, `OP`, `DEC_NUMBER`; see the diagnosis) provably *cannot* be
rewritten to a plain `regex` string, so under elimination-alone they stay on the slow
`fancy-regex` side-probe forever. The only way to give *them* single-pass speed and
bakeability is a combined automaton we build ourselves — and because their assertions
are **bounded** (hence regular, hence lowerable into ordinary states), a DFA suffices.

A second consequence makes the lowering the *preferred* route even for Tier-E: lowering
into the automaton means the **bundled grammars stay byte-verbatim upstream** — no
hand-edited regexes, none of the faithfulness/maintenance drift the memo flagged
(axis 3). The grammar-rewrite shortcut is dropped.

## Phases

L0 and L1 **landed in PR #114** — the `ScannerBackend` `match_at` seam, a
`regex-automata` multi-pattern `DfaScanner` behind `LexerBackend::Dfa`, the
maximal-munch driver, and the differential oracle `tests/test_scanner_differential.rs`
(Regex vs Dfa over the bank + JSON corpus + Python files, lookaround-free grammars).

> **One mechanism, no grammar edits.** Earlier drafts split "edit the Tier-E grammars"
> from "lower the Tier-G guards." That split is **dropped.** The Tier-E/Tier-G
> distinction is only about whether an equivalent *regex string* exists; at the
> **automaton** level it dissolves — every bounded assertion (both tiers) lowers to DFA
> states the same way. So the bundled grammars are **not** hand-rewritten. *(Optional:
> a load-time regex-string substitution could land the Tier-E win on the `Regex`
> backend before the Dfa flip — not required, not the default plan.)*

### L2 — The bounded-lookaround lowering feature *(the meat)*

> **Status — all three lowering shapes have landed (M0–M3).** The harness-first net
> exists, and every supported shape now lowers into the DFA behind it, gated green:
> * **Front-end** resurrected from closed #110 (without its Pike-VM `matcher.rs`):
>   `src/lookaround/mod.rs` (assertion parser) + `src/lookaround/classify.rs` (the
>   shape classifier) + `src/lookaround/lower.rs` (the real lowering). `lower_terminal`
>   lowers a fully-supported terminal into per-branch sub-patterns + guards, **declines**
>   (routes to `fancy-regex`) an instance it cannot faithfully lower (a non-greedy-
>   monotone guarded base, a variable-offset lookbehind, a lookaround nested behind a
>   flag wrapper), and **rejects** an out-of-shape assertion permanently.
> * **Engine** (M0 re-platform): the `DfaScanner` is rebuilt on `thompson::Builder` →
>   `dense::DFA`, with a **leftmost-first plain engine** (unguarded sub-patterns) and an
>   **all-matches guarded engine** (the guarded-accept accumulator). M1 landed the
>   trailing-boundary guarded accept, M2 the leading-boundary precondition, **M3 the
>   bounded lookbehind** — a *backward* guard checked at a **fixed char-offset** from the
>   match start (the offset is constant because the lowering declines a variable-width
>   prefix), so the history window is read directly from the haystack at lex time, the
>   "carry the window forward" move realized without a custom NFA.
> * **Harness layers, now all active**: the scanner differential
>   (`tests/test_scanner_differential.rs`) compares the generated lookaround-grammar
>   population under both backends — 0 pending, ~409 grammars / 1M+ inputs, 0
>   divergences; generative equivalence vs `fancy-regex` for all three shapes
>   (`tests/test_lowering_equivalence.rs`) plus the boundary **and** lookbehind
>   equivalence-layer mutation meta-tests (ignore-the-lookbehind / forget-the-parity-flip
>   / off-by-one-width, each caught); the Route-1 DFA-equivalence proof for all three
>   shapes (`tests/test_lowering_proof.rs`); the reject corpus + mutation meta-test
>   (`tests/test_lowering_reject.rs`); and the seam/edge fixtures
>   (`tests/test_lowering_fixtures.rs`). Generators + oracle + mutation framework live in
>   `tests/common/lowering.rs`.
>
> **Still on the `fancy-regex` side-probe (a *decline*, not a gap):** a bounded
> lookbehind after a **variable-width** prefix — `python.LONG_STRING`'s
> `.*?(?<!\\)(\\\\)*?` and `python.STRING`'s `(?<!\\)` — has no fixed offset, so M3
> declines it (correct, never mis-lowered). `python.STRING`'s leading `(?!"")` after a
> variable-width prefix needs NFA-state splicing. Both are the deferred variable-offset /
> STRING-splice milestone; until then those terminals lex on `fancy-regex` exactly as
> before, so the bundled `python.lark`/`lark.lark` stay correct.

A **general** lowering keyed on the assertion's **shape**, not on the six bundled
terminals. Lower each supported bounded assertion into lookaround-free DFA states
("How the lowering works"), fold all terminals into one `regex-automata` multi-pattern
NFA → DFA, and drive it with the maximal-munch loop (extended for trailing guards).
Bundled `STRING`/`OP`/`DEC_NUMBER`/`LONG_STRING`/`REGEXP` are just instances; **any
user grammar using a supported shape works too**; unsupported assertions are **rejected
at build time** with a clear, actionable error. Grammars stay verbatim. This is a real
feature — built **harness-first, one shape at a time**, gated by the verification
harness (see Process).

**L2 re-platforms the `DfaScanner` engine — it is *not* additive over L1.** L1's
`DfaScanner` is `meta::Regex::new_many`, whose only input is **pattern strings**, and
`regex-automata` categorically cannot represent `(?!…)` (the reason `fancy-regex`
exists). The lowered G-tier cannot ride `new_many` even in principle: `STRING`'s leading
guard has *no* plain-string form (the definition of G-tier), and a guarded accept is a
driver/automaton-level construct, not a pattern. So L2 must drop below the meta engine —
**hand-assemble the lowered fragments with `thompson::Builder`, compile the plain
terminals' HIR, union them into one NFA, and determinize a `dense`/`hybrid` DFA we drive
through the `Automaton` trait** (the same lower layer the #35 collision check already
uses). *(Tier-E lowerings are plain strings and could stay on `new_many`, but the
G-tier forces the re-platform, so everything moves to the hand-built construction.)*
Two fallouts to carry forward, both gated by the differential oracle:

* **Re-validate the leftmost-first tie-break** on the new construction — the
  `dfa_tiebreak_*` / `dfa_priority_and_width_ordering` tests were written against the
  meta union and must be re-established against the hand-built DFA.
* **Re-derive the start-byte prefilter** — `plain_start_bytes` is computed off the meta
  union today; it must be recomputed from the new union (or the common path regresses).

### L3 — Flip the Dfa backend to default

Once L2's lowering is green across the full differential bank, make `LexerBackend::Dfa`
the default. The throughput + correctness wins for the lookaround grammars
(`python`/`lark`) land here.

### L4 — Remove `fancy-regex` from the runtime

With every terminal on the DFA, drop the `AnyRegex::Fancy` runtime routing — the lexer
is `regex-automata`-only. **Keep `fancy-regex` as a dev/test dependency**: it remains
the independent match-length oracle the lowering is verified against. This is a
standing decision, not a temporary state.

### L5 — Bake the DFA static (the bakeability payoff)

Serialize the combined DFA (`regex-automata` `to_bytes`) + the small guarded-accept
side-table, and bake them into the standalone / C / WASM runtimes, replacing the baked
`ScannerPlan` alternation. Confirm the bundled `python`/`lark` grammars now generate
standalone parsers.

## How the lowering works

A DFA's only memory is its current state, so it can enforce any condition over a
**bounded window** of characters — and every supported assertion looks at a fixed,
finite window. Three shapes, three moves:

* **Bounded lookbehind** (`LONG_STRING`'s `(?<!\\)(\\\\)*?`) → **carry the window
  forward in the state.** Track the needed history (here, backslash-run parity) as you
  scan; gate the relevant edge on it. A finite (e.g. 2×) state duplication. Easiest
  case — you move *toward* the lookbehind.
* **Leading boundary** (`STRING`'s `(?!"")`) → **splice in branch states** that peek the
  next ≤k chars; the forbidden continuation leads to a dead (non-accepting) state. Pure
  NFA construction.
* **Trailing boundary** (`OP`'s `(?![a-z])`, `DEC_NUMBER`'s `(?![1-9])`) → a **guarded
  accept.** The lookahead char belongs to the *next* token, and the maximal-munch
  driver is already about to read it, so tag the accept "valid only if the next byte ∉
  S" and have the driver record the accept only when that holds. The length-changing
  case (`DEC_NUMBER`: `0001`→`00`) follows from maximal munch remembering the *last
  accept where the guard held* — no backtracking engine.

  **Caveat — guarded accept × multi-pattern priority is an up-front design item, not
  "free."** "Falls out for free" holds only for a terminal *in isolation*. In the
  combined automaton, one state accepts for several patterns with **different** guards
  (`[a-z]` for `OP`, `[1-9]` for `DEC_NUMBER`), and a failing guard can invalidate the
  engine's leftmost-first winner — at which point the correct token is a **runner-up**
  that a single-`Match` API never surfaces. So the driver needs a **per-pattern
  guarded-longest accumulator** over the **accept-set** at each state, then a post-hoc
  Lark `(priority, length)` selection across the survivors — an `Automaton`-level view
  of the accepting pattern set, *not* a single `PatternID`. (This is a second,
  independent reason `meta::Regex::new_many` can't host the lowering — it couples to the
  L2 re-platform above.) Tractable, and the differential oracle catches regressions, but
  it must be designed in from the start.

**General backstop.** For anything the three moves don't cover directly, the rigorous
fallback is closure theory: a bounded assertion is a regular constraint, and finite
automata are closed under intersection/complement, so it can be intersected into the
NFA by **product construction** — the same machinery already in `src/lexer.rs` for the
#35 collision check. (Recognition is fully general this way; priority-correct
*match-length* for arbitrary internal assertions is the hard residue — see boundary.)

**Pipeline.** parse the terminal regex → identify assertion nodes + positions (salvage
PR #110's `src/lookaround/` front-end) → classify + bound-check (unbounded → reject) →
lower (NFA fragments + guarded-accept side-table entries) → union all terminals →
determinize (`regex-automata`) → maximal-munch driver consults the guard table. "Bake
into the DFA" = the determinized table + the tiny guard side-table, both static data.

## What we support — the verifiability boundary

The supported set is defined by **what we can independently verify**, not by what's
convenient to code:

* **Supported (lowered):** fixed-position, fixed-width boundary assertions — leading
  `(?!S)`/`(?=S)`, trailing `X(?!S)`/`X(?=S)`, and bounded lookbehind `(?<!…)`/`(?<=…)`.
  This covers the bundled six **and** the census's real user-grammar classes
  (reserved-word exclusion, `=(?!=|>)`, `:(?!:)`, fixed-width lookbehind, …) — so it is
  a general feature, a strict expansion over the old eliminate-and-reject plan.
* **Rejected (loud build error):** unbounded-width assertions (`(?![ ]*X)`), and
  internal, priority-entangled bounded assertions where match-length under greedy/lazy
  priority is not reproducible by a per-state guard — the memo's T3, which converges on
  a priority automaton (Pike-VM). Empirically empty in the ~40-grammar census, but
  rejected rather than guessed. The error names the terminal, shows the assertion, and
  suggests a fix.

The classifier's **dangerous** direction is *false-accept* (mis-lowering an unsupported
assertion). Its contract, enforced by the harness: **if it accepts and lowers, the
result MUST match `fancy-regex`; otherwise it MUST reject.**

## Verification harness

> *"AI/LLMs automate what you can verify."* — the feature is scoped to, and built
> against, what the harness can check. **The harness is the product; the lowering is the
> detail it pins down.**

**The linchpin — keep `fancy-regex` as a permanent test oracle.** It runs any bounded
lookaround correctly. We drop it from the *runtime* (L4) but retain it as a dev/test
dependency forever. It shares **no code** with the `regex-automata` lowering, so a test
cannot pass for the wrong reason. The master invariant:

> for every grammar and input `s`: `DfaScanner(lowered).lex(s)` **==** today's
> `Scanner(regex + fancy-regex).lex(s)`.

This is the #114 differential oracle **extended from lookaround-free to lookaround
grammars** — the reference side keeps `fancy-regex`. It tests the whole integration
(maximal munch, priority, `unless`, `%ignore`, contextual narrowing, the trailing
rewind) against a trusted reference over the 512-grammar bank.

**Layers (broad net → airtight spot-checks):**

1. **Scanner-level differential (master).** `DfaScanner(lowered)` vs `Scanner(fancy)`
   over the compliance bank, JSON corpus, capped Python files, **and a generated
   grammar population**. Token-stream + error-position equality.
2. **Terminal-level *generative* equivalence vs `fancy-regex`.** For each supported
   shape, *generate* hundreds of concrete terminals (vary base pattern, char-set,
   width, content) and compare lowered vs `fancy-regex` over exhaustive small-alphabet
   corpora (and the quotient-alphabet sufficiency bound where feasible). Coverage stops
   depending on whose imagination — the lesson from missing `DEC_NUMBER`'s length-change
   until it was *run*.
3. **Route-1 DFA-equivalence proof.** For the bundled six + per-shape representatives,
   the decidable product-equivalence — "proven, no counterexample." **Per-shape proof
   obligation:** a shape is not "supported" until its representative proof is committed.
4. **Reject corpus (the dangerous direction).** Generate *out-of-shape* assertions
   (unbounded, internal/priority-entangled, backref, nested, variable-width behind) and
   assert each is **rejected**, never lowered.
5. **End-to-end Python-Lark matrix.** `test_lookaround` (parser×lexer) + `test_stdlib`
   + new user-grammar fixtures via `generate_oracles.py`.

**Validate the harness itself — mutation meta-test.** A committed set of
deliberately-wrong lowerings (forget the parity flip; invert the trailing-guard set;
off-by-one width; drop the EOF case; accept zero-width) — a meta-test asserts **every
mutant is caught** (some layer goes red). A surviving mutant = a hole in the net = build
failure. This is what makes the net trustworthy enough to delegate the implementation
to, and it defends against a test being silently weakened.

**Seam/edge checklist the generators must hit:** trailing guard at EOF; empty/zero-width;
maximal-munch competition (`OP` vs `RULE`); `unless` over a lowered terminal; `%ignore`
+ contextual narrowing; newline/DOTALL bodies; UTF-8 byte boundaries (the DFA is
byte-level, terminals are char-level); `g_regex_flags`; `PatternID` leftmost-first
priority surviving the union.

**Process (how this is built safely):**

1. **Harness-first.** Build all oracles + generators + the mutation meta-test **before**
   the lowering, with the lowering stubbed to *reject everything* (the differential
   oracle stays green on `fancy-regex`; the generative layers are pending). The net
   exists before the risky code.
2. **One shape at a time.** Trailing-boundary first (the self-contained guarded-accept),
   then leading-boundary, then bounded-lookbehind. A shape ships only when its full gate
   is green: generative equivalence + route-1 proof + reject corpus + scanner
   differential + mutation meta-test.
3. **Machine-enforced rigor.** Every gate is an independent, machine-checkable oracle —
   not reviewer trust. "Safe to merge" is answered by CI.
4. **Deterministic + never-panic.** Fixed seeds / exhaustive enumeration so failures
   reproduce; a robustness fuzzer asserts the classifier never panics and never silently
   mis-lowers on arbitrary bounded patterns — lower-correctly or reject-cleanly.

## Risks / open questions

* **Classifier false-accept** is the highest-severity failure (silent mis-lower).
  Mitigated by the contract test (accept ⇒ matches `fancy-regex`, else reject) + the
  reject corpus + the mutation meta-test.
* **Defining the supported/rejected boundary precisely** — which internal assertions are
  still per-state-guardable vs Pike-VM-shaped — is itself design work in L2; when
  unsure, **reject**.
* **UTF-8 / byte-vs-char** — `regex-automata` DFAs are byte-level; the lowering and the
  maximal-munch driver must respect char boundaries. Explicit seam-checklist coverage.
* **Determinization blow-up** from lowering assertions (parity duplication + spliced
  branches) on top of python.lark's many per-state contextual scanners. The **lazy
  (hybrid) DFA** mitigates this at *runtime* (states built on demand) — but **L5 bakes
  via `to_bytes`, which needs a fully-determinized `dense` DFA**, so the bake target
  pays the determinization the lazy path never does. The lazy mitigation therefore does
  **not** cover the bake. Gate it: a `perf-counters` **dense build-cost gate** (a
  codegen-time cost, paid at standalone generation, not every runtime load), matching
  the Earley/CYK scaling gates, so a determinization regression is caught deterministically.
* **Tie-break fidelity** — Lark's (priority, length, …) selection + `unless` on top of
  raw `PatternID`. The differential oracle is the net.
* **Lost free optimizations** — the regex crate's auto-prefilters; must be re-added
  explicitly (L1 carried this) or the common path regresses.
* **Maintenance surface** — the lowering pass + shape handlers. Bounded, oracle-gated,
  and per-shape-proven, but real; the cost consciously accepted in the strategy reversal.

## Salvage map (from closed PR #110)

| Artifact | Disposition |
|---|---|
| `src/lookaround/mod.rs` (assertion front-end) | **Resurrect** from the closed #110 branch / git history — it is **not** on `master`, so retrieving + re-landing it is a real first task — then repurpose as the L2 lowering/classifier pass |
| `src/lookaround/matcher.rs` (Pike-VM) | **Not used** — a DFA replaces it |
| `tests/test_lookaround.rs` + `fixtures/oracles/lookaround/` | **Reuse** as the lookaround behavioral gate |
| `fancy-regex` (runtime routing) | **Drop at L4 — retain as the test oracle** (Verification) |
