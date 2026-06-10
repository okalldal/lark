# Lexer DFA plan — one combined automaton over all terminals

*Status: active umbrella plan for the lexer's lookaround/throughput work.*
*Supersedes the framing of [`LOOKAROUND_ELIMINATION_PLAN.md`](LOOKAROUND_ELIMINATION_PLAN.md),
which becomes **Phase 1** here. Rationale and the decision reversal are recorded in
[`LOOKAROUND_STRATEGY_ANALYSIS.md`](LOOKAROUND_STRATEGY_ANALYSIS.md) (revision
2026-06-08). Terminal-level classification in
[`TERMINAL_REDUCTION_DIAGNOSIS.md`](TERMINAL_REDUCTION_DIAGNOSIS.md).*
*Date: 2026-06-08 (status refreshed 2026-06-09).*

## Current status on master

A one-paragraph honest snapshot, so the plan stops describing landed work as future.
The per-terminal route table lives in [`LEXER_DFA_STATUS.md`](LEXER_DFA_STATUS.md).

| Phase / piece | State |
|---|---|
| L0/L1 — `ScannerBackend` seam + `regex-automata` `DfaScanner` + maximal-munch driver | **Landed** (PR #114). |
| M1 — trailing-boundary guarded accept | **Landed.** |
| M2 — leading-boundary precondition | **Landed.** |
| M3 — fixed-offset bounded lookbehind | **Landed.** |
| M4 — `python.STRING` opening-guard splice (`recognize_string_idiom`) | **Landed** (PR #124). |
| L3 — `LexerBackend::Dfa` as the default backend | **Landed** (it is `#[default]`). |
| `lark.REGEXP` lowering — Stage-B regex-literal idiom (`recognize_regexp_idiom`) | **Landed** (2026-06-10). |
| `python.LONG_STRING` lowering — Stage-B long-string idiom (`recognize_long_string_idiom`) | **Landed** (2026-06-10). |
| Flag-wrapper strip — terminal `/…/is` flags reach the lowering (`strip_whole_pattern_flag_wrapper`) | **Landed** (2026-06-10, with the LONG_STRING idiom). The loader bakes terminal flags into the pattern as `(?is:…)` with `PatternRe.flags = 0`; before the strip, the wrapped `python.STRING` silently rode the `Unsupported` compatibility fallback and a wrapped LONG_STRING the decline route **on the engine path** (the route-level proofs all held — on the unwrapped constants), invisible to the differential because the fancy reference agreed. The engine now strips the wrapper into the flag bitset before routing and re-applies it to every lowered branch/guard, so the M4/Stage-B idioms genuinely engage at runtime; pinned by `lexer::tests::dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe` (zero fancy side-probes). `g_regex_flags` DOTALL is likewise threaded into the lowering now (the global `(?s…)` prefix is prepended to lowered branches, so the lowering must see it). |
| L4 — remove runtime `fancy-regex` | **Landed** (2026-06-10). Both refusal arms (`Unsupported` AND `Declined`) are **categorized build errors** under the two-category scope taxonomy (`docs/LOOKAROUND_SCOPE.md`: `OutOfScope` vs `NotYetImplemented`, scoreboarded end-to-end by `tests/test_lookaround_scope.rs`); `fancy-regex` is an optional dependency behind the default-OFF, TEST-ONLY `fancy-oracle` feature (the `Regex` reference backend's historical probes for the L0 differential) plus the permanent dev-dependency oracle. Default builds have zero fancy code (`cargo tree -e normal`). The flip exposed and fixed two latent gaps: the loader's group-wrapped alternation arms (now normalized by `unwrap_vacuous_groups` — `(?:X) ≡ X` for whole-pattern/arm wrappers) and `python.DEC_NUMBER`'s guarded arm (admitted by the exact `is_leftmost_longest` semantic realizability gate), both of which had silently ridden the fallback. The Earley dynamic lexer and `unless` retyping run on per-terminal lowered matchers (`LoweredTerminalMatcher`). |
| L5 — bake a serialized DFA scanner bundle into standalone/C/WASM | **Unblocked by L4** — standalone still bakes the regex `ScannerPlan`; the scanner bundle is now fully serializable static data. |

So the strategy has paid off for the common path, for **every bundled lookaround
terminal** (`python.STRING`, `lark.REGEXP`, `python.LONG_STRING`, `python.DEC_NUMBER` —
all lowered, on the real engine path), and for the **drop-`fancy-regex`** win itself:
since L4 there is no runtime fallback engine — refusals are categorized build errors
(`docs/LOOKAROUND_SCOPE.md`) and default builds carry zero fancy code. The remaining
unrealized win is *bakeability* (L5): the standalone generator still emits a
regex-based `ScannerPlan`, not the serialized DFA bundle.

## Runtime routing taxonomy

Every terminal takes exactly one of these routes at build time. Keeping the names
distinct is what lets the docs, the classifier comments, and the tripwire test tell one
story:

* **Plain** — no lookaround assertion. Compiles straight into the leftmost-first plain
  DFA. The overwhelming common case.
* **Lowered** — a supported, *proven* bounded assertion (M1/M2/M3/M4). Lowered into plain
  DFA branches + guard side-tables; **no** `fancy-regex` at runtime for this terminal.
* **Declined** — a per-instance lowering the realizability check declines (a
  variable-offset lookbehind outside a recognized idiom, a base the `is_leftmost_longest`
  semantic gate cannot prove), or a pattern the analyzer cannot handle. Since L4 a
  **categorized build error**, typed by `classify::DeclineReason` and mostly
  `Scope::NotYetImplemented` (`docs/LOOKAROUND_SCOPE.md`) — a clean refusal, never a
  mis-lowering. **No bundled terminal is here** — STRING, REGEXP, LONG_STRING and
  DEC_NUMBER all lower.
* **Rejected** — an out-of-shape assertion (unbounded, internal/priority-entangled,
  backref, nested, variable-width lookbehind). A categorized build error, mostly
  `Scope::OutOfScope` — **permanently** (the scoreboard asserts these as the contract).
* **Invalid regex** — neither `regex` nor the lookaround analyzer accepts the pattern.
  A build error at grammar load (`PatternRe::new`).

**The policy is enforced (L4 landed).** The typed split is in code
(`classify::route_terminal_dotall` → `LoweringRoute::{Plain, Lowered, Declined,
Unsupported, Invalid}`) AND in the runtime policy: every refusal funnels through one
auditable seam (`lexer::route_fancy_only_terminal`, the successor of the historical
`push_fancy_fallback` compatibility seam) and becomes a categorized
`GrammarError::LookaroundScope` carrying `Scope` + the typed reason. The contract is
scoreboarded end-to-end by `tests/test_lookaround_scope.rs` (whose exhaustiveness
meta-test forces every refusal variant to a scoreboard row or a documented defensive
justification) and pinned on the engine path by
`tests/test_lowering_routes.rs::unsupported_user_lookaround_is_now_a_categorized_build_error`.

## Goal

Lex **every** terminal — lookaround-bearing ones included — in a **single
table-driven pass** over one combined automaton, built once and bakeable as static
data. Concretely: build the combined scanner on `regex-automata` (lazy/dense DFA,
multi-pattern `PatternID`), **lower** each *supported, proven* bounded lookaround
assertion into lookaround-free automaton states so it joins the same machine (declining or
rejecting the rest — see "What we support" and the red line under "Future
generalization"), drive it with a maximal-munch loop that reproduces Lark's exact
selection, and drop `fancy-regex` from the runtime.

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

> **Status — all three lowering shapes (M0–M3) *and* the `python.STRING` opening-guard
> splice (M4) have landed.** The harness-first net exists, and every supported shape now
> lowers into the DFA behind it, gated green:
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
> **M4 — the `python.STRING` opening-guard splice (landed).** `python.STRING`'s
> `(?!"")` sits after a **variable-width** prefix (`[ubf]?r?|r[ubf]`) + the opening
> quote — an internal/variable-position leading boundary M2's fixed-offset guard cannot
> host. It is now lowered by the **NFA-state splice** the plan calls for
> (`src/lookaround/lower.rs::recognize_string_idiom`): the lazy escaped body
> `.*?(?<!\\)(\\\\)*?<q>` is normalized *internally* to its proven greedy
> character-class equivalent (the Type-A rewrite `matchlen` justifies — this **absorbs**
> the `(?<!\\)` lookbehind, so M3's variable-offset decline does not apply), and the
> `(?!"")` reduces — exactly, because the normalized body can never *begin* with the
> delimiter — to an empty/non-empty arm split with a trailing `(?!")` guard on the
> empty arm (the only place the assertion's window over-reaches the matched token). The
> empty arm's base `<prefix>""` is *prefix-free*, so the guarded longest-accept
> accumulator reproduces fancy's match (the realizability check now admits prefix-free
> bases alongside greedy-monotone ones). Gated by: the hand-authored `""""`/`"" ""`
> adversarial canary under the default `Dfa` backend (`tests/test_string_splice.rs`),
> the state-pruned Route-1 proof on the **real nested shape**
> (`tests/test_lowering_proof.rs::route1_proof_string_idiom_real_nested_shape`), the
> generative-equivalence layer + the drop-the-`(?!"")`-guard mutant
> (`tests/test_lowering_equivalence.rs`), and the python.lark differential (0
> divergences with STRING *lowered*, not declined).
>
> **The Stage-B regex-literal idiom (`lark.REGEXP`) is lowered too** (2026-06-10):
> the internal `(?!\/)` after the opening slash reduces — exactly, because the close
> (`/`) and every body alternative (`\/`, `\\`, `[^/]`) start with disjoint chars — to a
> non-empty-body bump on the lazy repetition (`*?` → `+?`). One **unguarded**
> lookaround-free branch joins the leftmost-first plain engine, whose native priority
> semantics reproduce the lazy close, the dangling-escaped-slash backtracking close
> (`/a\/b` → `/a\/`), and the greedy `[imslux]*` flags suffix. Recognizer:
> `src/lookaround/lower.rs::recognize_regexp_idiom` — exact-shape only. Gated by: the
> hand canaries under the default `Dfa` backend (`tests/test_regexp_splice.rs`), the
> slash/backslash-heavy generative equivalence + the drop-the-`+?`-bump mutant
> (`tests/test_lowering_equivalence.rs`), the state-pruned Route-1 proof on the real
> bundled shape (`tests/test_lowering_proof.rs::route1_proof_regexp_idiom_real_shape`),
> and the scanner-differential population + lark.lark file corpus.
>
> **The Stage-B long-string idiom (`python.LONG_STRING`) is lowered too** (2026-06-10):
> the lazy escaped body + escape-parity close `.*?(?<!\\)(\\\\)*?"""` is normalized to
> lazy escape-pair items `(?:[^\\<nl>]|\\.)*?"""` (`<nl>` excluded iff not DOTALL) — a
> backslash can only be consumed as the start of a pair, so item boundaries fall exactly
> at the even-backslash-parity positions the `(?<!\\)(\\\\)*?` close demands, and the
> kept lazy `*?` picks the first such `"""` on both sides (the committed Type-A finding
> `tests/test_lookaround.rs::long_string_match_length_equivalence`). Two **unguarded**
> per-arm branches join the leftmost-first plain engine; no multi-char delimiter
> automaton was needed (a lone `"` in the body simply doesn't close — laziness picks the
> first full triple). Recognizer: `src/lookaround/lower.rs::recognize_long_string_idiom`
> — exact-shape only (`"""`/`'''` delimiters). Gated by: the hand canaries + the
> exhaustive dotall backend differential (`tests/test_long_string_splice.rs`), the
> generative equivalence + parity/two-quote/greedy mutants
> (`tests/test_lowering_equivalence.rs`), the state-pruned Route-1 proof representative
> (`tests/test_lowering_proof.rs::route1_proof_long_string_idiom_real_shape` — see its
> completeness scope note; the committed Type-A equivalence pin is the primary basis,
> per the checklist's "or an equivalent stronger oracle" alternative), the
> scanner-differential population + python.lark docstring corpus, and the stdlib oracles.
>
> **The flag-wrapper strip made the idioms real on the engine path** (2026-06-10): the
> loader bakes terminal `/…/is` flags into the pattern (`(?is:…)`, `PatternRe.flags = 0`),
> so the router used to see every assertion nested inside a `Group` — the wrapped
> `python.STRING` silently rode the `Unsupported` compatibility fallback at runtime
> (every M4 proof held, but on the unwrapped constants; the differential could not see
> it because the fancy reference agreed by construction). `DfaScanner::build` now strips
> a whole-pattern flag wrapper back into the flag bitset before routing
> (`strip_whole_pattern_flag_wrapper`) and re-applies it to every lowered branch and
> guard; `g_regex_flags` DOTALL is threaded into the lowering the same way. Pinned by
> `lexer::tests::dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe` (the three
> bundled idioms build with **zero** fancy side-probes) and the
> `newline_dotall_body` / `g_regex_flags_dotall_long_string` seam fixtures.
>
> **No bundled terminal rides the `fancy-regex` side-probe any more.** The probe stays
> in the runtime only for per-instance user declines and the `Unsupported` compatibility
> fallback — the L4 policy flip is the remaining gate.

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

### L3 — Flip the Dfa backend to default *(landed)*

**L3 has landed.** `LexerBackend::Dfa` is `#[default]`, so `LexerConf::new` /
`LarkOptions` build the DFA scanner unless `LexerBackend::Regex` is explicitly chosen,
The differential oracle is 0 divergences across the full bank + JSON + python/lark
corpora, so the swap is correctness-identical, and it is faster on the all-plain common
path. The fallback has since been eliminated entirely (L4): refusals are categorized
build errors, and the differential's fancy reference lives behind the TEST-ONLY
`fancy-oracle` feature.

### L4 — Remove `fancy-regex` from the runtime *(landed 2026-06-10)*

The lexer is `regex-automata`-only. Both refusal arms are **categorized build errors**
under the two-category scope taxonomy — see **`docs/LOOKAROUND_SCOPE.md`** (the policy
document) and `tests/test_lookaround_scope.rs` (the machine-checked scoreboard):

* `Unsupported` (out-of-shape) → mostly `Scope::OutOfScope` — by-design non-goals,
  asserted as the contract (general internal lookahead, with the audited
  delimited-token idioms as the sanctioned growth path; variable-width lookbehind,
  which Python `re` also rejects — parity; backrefs/backtracking-only syntax — the
  named parity break; degenerates).
* `Declined` (per-instance) → mostly `Scope::NotYetImplemented` — clean conservative
  refusals that double as **promotion tripwires** (variable-offset lookbehind,
  non-realizable guarded bases, VERBOSE wrappers, interior-group assertions).

What the flip surfaced and fixed (the same model-vs-reality class the flag-wrapper
strip closed for STRING): the loader wraps terminal-algebra alternation arms in
`(?:…)`, so arm-end trailing guards were misread as group-nested internal assertions —
now normalized by `classify::unwrap_vacuous_groups` (`(?:X) ≡ X` for whole-pattern/arm
bare wrappers, provably neutral); and `python.DEC_NUMBER`'s guarded arm base
`0(?:_?0)*` failed both syntactic realizability fast paths — now admitted by
`lower::is_leftmost_longest`, the **exact** semantic decision (LeftmostFirst × All
product-DFA walk: leftmost-first ≡ longest on every input), audited by unit pins +
exhaustive generative equivalence vs the fancy oracle + the stdlib oracles.

Runtime seams that lost fancy: `DfaScanner` (side-probe deleted), the `Regex`
reference `Scanner` (default build: lowered per-terminal side-probes;
**`fancy-oracle` feature**: the historical `\G` fancy probes, TEST-ONLY, for the L0
differential), the Earley `DynamicMatcher` (per-terminal `LoweredTerminalMatcher` —
single-terminal `DfaScanner`s), `unless` retyping (anchored lowered branches +
guards), and `PatternRe::new` (load-gate = `regex` ∪ lookaround-analyzer parse).
**`fancy-regex` stays as a dev/test dependency forever** — the independent
match-length oracle (this was always the standing decision) — plus the optional
`fancy-oracle` feature for the whole-lexer reference. CI runs both matrices
(`cargo test --all` and `cargo test -p lark-rs --features fancy-oracle`).

### L5 — Bake the scanner bundle static (the bakeability payoff) *(blocked)*

The bake target is **not** literally one serialized DFA. The implemented scanner is a
*bundle*, and L5 must serialize the bundle the implementation actually has:

* the **plain leftmost-first** dense DFA (unguarded sub-patterns),
* the **guarded all-matches** dense DFA (guarded sub-patterns),
* the **guard body** DFAs (or serialized guard tables) for leading/trailing guards,
* the **lookbehind guard** side tables (offset + width + body DFA),
* the **pattern / rank / branch-order** maps that drive leftmost-first selection,
* the **start-byte prefilter**,
* the **`unless`** keyword-retype map,
* the **`%ignore`** set.

Bake that bundle into the standalone / C / WASM runtimes, replacing the regex
`ScannerPlan` alternation, starting with **Rust standalone** before C/WASM. Confirm the
bundled `python`/`lark` grammars then generate standalone parsers. **Unblocked: L4 has
landed** (the bundle is fully serializable static data — no fancy side-probe exists);
the remaining work is the standalone generator itself, which today still bakes the
regex `ScannerPlan` (see `src/standalone/mod.rs`).

## How the lowering works

A DFA's only memory is its current state, so it can enforce any condition over a
**bounded window** of characters — and every supported assertion looks at a fixed,
finite window. Three shapes, three moves:

* **Bounded lookbehind** (`LONG_STRING`'s `(?<!\\)(\\\\)*?`) → **carry the window
  forward in the state.** Track the needed history (here, backslash-run parity) as you
  scan; gate the relevant edge on it. A finite (e.g. 2×) state duplication. Easiest
  case — you move *toward* the lookbehind. (In practice the bundled LONG_STRING never
  carries a window at all: the Stage-B idiom's escape-pair body normalization absorbs
  the parity into the branch regex itself.)
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
  **not** cover the bake. **Gated (landed):** the `perf-counters` **dense build-cost
  gate** — `tests/test_lexer_dfa_build_scaling.rs` keys on the `dense_build_bytes` work
  counter (summed `dense::DFA::memory_usage` over a scanner build) and asserts the
  determinized size stays flat *per terminal* and *per guard width* over a size sweep,
  matching the Earley/CYK scaling gates. It is a codegen-time cost (paid at standalone
  generation, not every runtime load), so a determinization regression — parity
  duplication, a spliced/product union — is caught deterministically. CI runs it as its
  own `--features perf-counters` step.
* **Tie-break fidelity** — Lark's (priority, length, …) selection + `unless` on top of
  raw `PatternID`. The differential oracle is the net.
* **Lost free optimizations** — the regex crate's auto-prefilters; must be re-added
  explicitly (L1 carried this) or the common path regresses.
* **Maintenance surface** — the lowering pass + shape handlers. Bounded, oracle-gated,
  and per-shape-proven, but real; the cost consciously accepted in the strategy reversal.

## Salvage map (from closed PR #110)

| Artifact | Disposition |
|---|---|
| `src/lookaround/mod.rs` (assertion front-end) | **Landed** — resurrected from closed #110 and repurposed as the L2 classifier/lowering front-end (`mod.rs` parser + `classify.rs` + `lower.rs`), without its Pike-VM `matcher.rs` |
| `src/lookaround/matcher.rs` (Pike-VM) | **Not used** — a DFA replaces it |
| `tests/test_lookaround.rs` + `fixtures/oracles/lookaround/` | **Reuse** as the lookaround behavioral gate |
| `fancy-regex` (runtime routing) | **Drop at L4 — retain as the test oracle** (Verification) |

## Future generalization without a Pike VM

This is the **next safe expansion path** — documented, not implemented in this cleanup.
It defines what can be lowered *without* building a priority regex engine. The expansion
ladder is four named stages; cross the red line at the end only as a deliberately-approved
future phase.

### Stage A — the general `GuardAt` model

The next safe generalization is not "arbitrary lookaround." It is a uniform
representation for assertions whose evaluation point is **uniquely determined from the
candidate match span**.

Conceptual model:

```text
GuardAt {
    source: original assertion source, for diagnostics,
    polarity: positive | negative,
    direction: ahead | behind,
    at: start + k_chars | end - k_chars | end + 0,
    body: assertion-body regex source,
    max_width_chars: finite width of the assertion body,
}
```

For a candidate token match spanning byte range `[start, end)`:

- `start + k_chars` means: advance `k` Unicode scalar values from `start`.
- `end - k_chars` means: walk `k` Unicode scalar values backward from `end`.
- `end + 0` is the normal trailing-lookahead position.
- The resolved guard point must be a valid UTF-8 boundary.
- A lookahead guard tests `body` anchored at the guard point.
- A lookbehind guard tests whether `body` fully matches some bounded suffix ending at the guard point.
- A positive guard requires the assertion body to match.
- A negative guard requires the assertion body **not** to match.

A lowered branch is:

```text
Branch {
    base_regex: lookaround-free consumed-token regex,
    guards: Vec<GuardAt>,
}
```

Multiple guards per branch are allowed if every guard point is computable from the
candidate match span and every guard body is bounded.

Supported examples:

```text
(?=if)[A-Za-z_]+
    => GuardAt { direction: ahead, at: start + 0, positive, body: "if" }

[0-9]+(?![1-9])
    => GuardAt { direction: ahead, at: end + 0, negative, body: "[1-9]" }

abc(?=:) [A-Za-z]+
    => GuardAt { direction: ahead, at: start + 3, positive, body: ":" }
    if the prefix before the assertion is fixed-width.

[A-Za-z]+ (?=:) :
    => GuardAt { direction: ahead, at: end - 1, positive, body: ":" }
    if the suffix after the assertion is fixed-width.

[A-Za-z](?<!_)x
    => GuardAt { direction: behind, at: start + 1, negative, body: "_" }
```

**Acceptance rule.** An assertion may lower to `GuardAt` only when its position is fixed
relative to `start` or `end` for **every** accepting path of the branch. If the assertion
position depends on a variable-width prefix, an ordered alternative, or a lazy/greedy path
choice, it must be rejected or declined unless a separate proof-backed lowering proves
exact match-end equivalence.

**Branch splitting** is allowed only when it preserves Python/Lark leftmost-first branch
order exactly. Splitting must not silently turn an internal priority problem into
unordered alternatives.

**Guarded branch realizability.** If a guard can invalidate some candidate match lengths,
the base branch must be known to reproduce Python/Lark match-end semantics under the
guarded-accept driver. Acceptable reasons include:

- greedy-monotone base,
- prefix-free base,
- or a committed machine proof of match-end equivalence.

Otherwise, decline/reject. Do not guess. (This is exactly the `is_guard_realizable` gate
the M1–M4 lowerings already use — Stage A generalizes the *position*, not the
realizability contract.)

### Stage B — the delimited-token idiom family

Separately from `GuardAt`, support a reusable family of audited **delimited-token**
lowerings:

```text
opener body close suffix?
```

Examples: short strings, long strings, regex literals, block comments.

These are **not** arbitrary internal-lookaround support. They are exact idioms where a
small delimiter automaton (KMP/Aho-Corasick-style, tracking how much of a multi-character
close delimiter has been seen, plus escape parity) can replace the lazy body +
escape/lookaround logic. **All three bundled instances have landed**:
`python.STRING`'s M4 splice was the first; `lark.REGEXP` (`recognize_regexp_idiom`,
where the internal `(?!\/)` reduces to a non-empty-body `*?`→`+?` bump) the second; and
`python.LONG_STRING` (`recognize_long_string_idiom`, where the escape-pair body
normalization absorbs the `(?<!\\)(\\\\)*?` parity close) the third. Notably neither
Stage-B idiom needed an explicit delimiter automaton — the leftmost-first plain engine's
native lazy semantics cover the multi-char `"""` close once the body items force escape
pairing. Each idiom must have (and each landed one has):

- an exact recognizer,
- a narrow acceptance surface,
- oracle equivalence against `fancy-regex`,
- a Route-1 / state-pruned proof or an equivalent stronger oracle,
- scanner differential coverage,
- hand-written seam canaries.

### Stage C — optional proof-backed product lowering

A future lowering may use product construction or another automata method **only as a
proof-backed path**:

1. propose a lowered branch + guard representation,
2. machine-prove match-end equivalence against the reference semantics,
3. accept only if the proof completes within deterministic size/time budgets,
4. otherwise decline/reject.

This is **not** a runtime fallback and **not** a license to accept arbitrary lookaround.

### Stage D — priority automaton / TDFA / derivative matcher *(future named phase only)*

Anything beyond Stages A–C — exact priority semantics for assertions whose position is
*not* uniquely determined — is a different implementation style, not a free lowering pass.
It is a **future named phase**, deliberately approved, never smuggled into a small
lowering PR. Do not accidentally rebuild the Pike VM under another name.

### The explicit red line

Reject or decline assertions inside **unbounded repetition**, and assertions whose
evaluation point depends on **ordered alternation, lazy quantifier choice, or other
priority-sensitive regex path history**.

Examples that must **not** be accepted by the `GuardAt` model unless a future
priority-automaton phase is deliberately approved:

```text
(?:X(?=Y))*          // assertion inside unbounded repetition
.*?(?=END)           // assertion point chosen by lazy search
(a|aa)(?=a)a         // assertion point depends on ordered alternative
(?:A|AB)(?!C)D       // only safe if split/proven without priority drift
```

Crossing this line means building a priority regex engine in another form: Pike VM,
Tagged DFA (TDFA), derivative matcher with priority semantics, or equivalent.

## Research direction / non-goals

- **Do not revive the Pike VM** (closed PR #110). The DFA route lowers bounded assertions
  *away*; it does not execute lookaround at runtime.
- **Language recognition** for a bounded lookaround is regular (finite automata are closed
  under intersection/complement). The hard part is exact **lexer match-end semantics**
  under greedy/lazy quantifiers and leftmost-first terminal priority — a plain language
  DFA is not enough (`/.*?END/` vs `/.*END/` recognize the same language but pick a
  different match end on `aENDbEND`).
- Going beyond **guard-at-fixed-position** (Stage A) and **audited delimiter idioms**
  (Stage B) likely becomes a priority-automaton / TDFA / derivative-matcher project
  (Stage D). Relevant literature: Barrière & Pit-Claudel (linear matching of JS regexes
  with lookaround); Varatalu/Veanes/Ernits and the RE# follow-up (derivative-based
  matching with intersection/complement/lookarounds); Trofimovich and Borsotti/Trofimovich
  (TDFA with lookahead, the RE2C lineage); Martynova & Okhotin (regex→DFA has inherent
  exponential worst cases — every "just determinize it" plan needs size gates).
- That must be a **named future phase**, not hidden inside small lowering PRs.

## Next implementation PR checklist

Any PR that lands a **new lowering** (a `GuardAt` generalization, a new idiom, …)
must include, in the *same* PR (the REGEXP idiom PR is the worked example):

- [ ] an **exact recognizer** with a narrow acceptance surface (reject-when-unsure);
- [ ] an explicit **route/status update** (move the terminal from *declined* to *lowered*
      in [`LEXER_DFA_STATUS.md`](LEXER_DFA_STATUS.md));
- [ ] **generative equivalence** vs `fancy-regex` for the new shape;
- [ ] a **Route-1 (or state-pruned) proof** representative, *or* a documented reason proof
      is infeasible plus an equivalent stronger oracle;
- [ ] **scanner differential** coverage (`tests/test_scanner_differential.rs`);
- [ ] **hand-authored canaries** for the specific adversarial seam;
- [ ] **reject-corpus** additions if the recognizer introduces a new false-accept risk;
- [ ] the **dense-DFA build-cost gate** still green
      (`tests/test_lexer_dfa_build_scaling.rs`);
- [ ] the **bundled-terminal status tripwire** updated
      (`tests/test_string_splice.rs::bundled_lookaround_terminal_lowering_status`);
- [ ] **docs + `CLAUDE.md`** updated in the same PR (and, if *all* bundled lookaround
      terminals now lower, the L4/L5 payoff re-evaluated).
