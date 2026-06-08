# Terminal-reduction diagnosis — the full lookaround census

*Status: diagnosis (input to the E2/E4 scope decision in
[`LOOKAROUND_ELIMINATION_PLAN.md`](LOOKAROUND_ELIMINATION_PLAN.md)).*
*Follow-up after PR #112 (E2a). Date: 2026-06-07.*

## Why this exists

E2a ([PR #112](https://github.com/okalldal/lark/pull/112)) proved three shapes —
`python.LONG_STRING`, the block-comment idiom (reducible), and `python.STRING`
(irreducible) — but left the plan's other three E2 candidates marked *"to be
verified"*: `lark.OP`, `lark.REGEXP`, and `python.DEC_NUMBER`. This note finishes
the classification of **every** lookaround terminal in the bundled grammars and uses
it to answer the open scoping question:

> Do all the irreducible terminals exhibit the same behavior, and what is the
> *smallest* engine addition that covers them — in particular, can it be done
> **without** the full Pike VM of the closed PR #110?

**Answer: yes, comfortably.** The irreducible surface collapses to a single shape —
a *fixed-width* boundary assertion — and, after grammar-level recovery, to a single
*terminal* (`STRING`). None of it needs the general thread-list/ε-closure Pike VM.

## Method

Each terminal is classified at two levels:

1. **Terminal level (decidable, oracle-free).** Run the original lookaround pattern
   on `fancy-regex` and a candidate lookaround-free rewrite on the `regex` crate, and
   compare the two anchored matched-prefix functions over an exhaustive corpus. This
   is the existing E2a `matchlen` harness (`tests/test_lookaround.rs`), now extended
   to `OP`/`REGEXP`/`DEC_NUMBER`. Both engines are leftmost-first/backtracking like
   CPython `re`, so the comparison is faithful and the result is a *proof*, not a
   sample.
2. **Grammar level (confirmed end-to-end).** Even when a guard is terminal-level
   irreducible, dropping it may not change the grammar's accept/reject — if the
   alternative tokenization is itself a parse error or is resolved by maximal munch.
   This is where `STRING` (genuinely irreducible) parts ways from `OP`/`DEC_NUMBER`
   (recoverable). This is now **proven**, not just reasoned: building the grammar
   with the guard removed yields byte-identical trees (or an identical reject) on
   every witness. Confirmed two ways — on **lark-rs itself** (the `recovery` tests in
   `tests/test_lookaround.rs`: guarded routes to `fancy-regex`, guard-free to
   `regex`) and independently on the **Python Lark oracle** (language- and
   tree-equivalence over the same witnesses) — so the result is triangulated.

## The complete bundled census

| Terminal | Grammar | Assertion | Terminal-level class | Pinned by |
|---|---|---|---|---|
| `LONG_STRING` | python | `(?<!\\)(\\\\)*?` lookbehind | **A — regex-rewritable** (proven equiv) | `long_string_match_length_equivalence` |
| `REGEXP` | lark | `(?!\/)` leading | **A — regex-rewritable** (proven equiv) | `regexp_match_length_equivalence` |
| block-comment | examples | `\*(?!\/)` | **A — regex-rewritable** (proven equiv) | `block_comment_match_length_equivalence` |
| `STRING` | python | `(?!"")` leading | **C — boundary-as-failure** (all-or-nothing, w=2) | `string_lookaround_free_rewrite_is_not_equivalent` |
| `OP` | lark | `(?![a-z])` trailing | **C — boundary-as-failure** (all-or-nothing, w=1) | `op_lookaround_free_rewrite_is_not_equivalent` |
| `DEC_NUMBER` | python | `(?![1-9])` trailing | **C — length-changing trailing** (w=1) | `dec_number_lookaround_free_rewrite_is_not_equivalent` |

(`STRING`'s body also carries the `(?<!\\)(\\\\)*?` lookbehind, but that part is
shown reducible by `LONG_STRING`; `STRING`'s *only* irreducible element is the
leading `(?!"")`.)

### Type A — regex-rewritable (no engine, no grammar change)

The assertion is pure redundancy once the body is constrained, so it rewrites to a
plain `regex` pattern with a *byte-for-byte identical* matched-prefix function:

* **`REGEXP`** `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*` → `\/(\\\/|\\\\|[^\/])+\/[imslux]*`.
  The `(?!\/)` only ever forbade the empty regex `//`; requiring a non-empty body
  (`+`) reproduces that, and the body alternation can never start with a bare `/`, so
  lazy `*?` and greedy `+` coincide. **Proven equivalent.**
* **`LONG_STRING`**, **block-comment** — proven in E2a.

These are ready to deploy in E4 with zero behavioral risk.

### Type C — the irreducible shapes (and how they differ)

All three Type-C members are **fixed-width** assertions (width ≤ 2) anchored at the
match boundary. They are *not* all the same behavior, and the difference is the whole
story:

* **`STRING` — leading, all-or-nothing.** `(?!"")` at a fixed offset (right after the
  opening quote). E2a proved it flips accept/reject: `""""` is a lex error but
  `"" ""` is two valid empty strings. A one-shot check of the two characters after
  the opening quote reproduces it exactly. O(1), no backtracking.
* **`OP` — trailing, all-or-nothing.** `[?](?![a-z])`: `?` matches *unless* a
  lowercase letter follows, in which case the whole terminal fails (length 1 → no
  match). The harness shows the divergence is exactly `(None, Some(1))` on `?[a-z]`
  and never drops an original match. A one-shot check of the single character after a
  candidate `?` reproduces it. O(1).
* **`DEC_NUMBER` — trailing, length-changing.** `(?![1-9])` is the only one that is
  *not* all-or-nothing: on `001`, fancy-regex backtracks the greedy zero-run to `0`
  (len 1) so the guard sees a `0`, while the guard-free rewrite takes `00` (len 2).
  This is the `a+(?!b)` trailing-lookahead family. The harness pins it as a
  **one-directional superset** (the rewrite never matches *less*) localized entirely
  to the leading-zero alternative. Reproducing this *at the terminal level* needs a
  fixed-width trailing assertion **plus** bounded backtracking of the preceding
  quantifier — more than a one-shot check, but still a narrow, fixed-width primitive,
  nowhere near the general Pike VM.

## Grammar-level recovery — the irreducible set shrinks to `STRING`

Terminal-level irreducibility does not imply the *grammar* needs the guard. Dropping
a Type-C guard only matters if it changes end-to-end accept/reject:

* **`STRING` — genuinely irreducible.** The alternative reading of `""""` is
  `STRING STRING` (two empty strings), which is *valid* to the parser, so the guard
  is the only thing that rejects `""""`. **Needs an engine primitive.** (Proven in
  E2a.)
* **`OP` — recoverable by maximal munch.** The guard exists so `?foo` lexes as the
  `RULE` token `/!?[_?]?[a-z][_a-z0-9]*/` (length 4) rather than `OP "?"` (length 1).
  Longest-match already prefers `RULE`, so the guard is redundant with the lexer's
  existing ordering. Drop it and rely on priority/length. **No engine.** *(Confirmed:
  guarded ≡ guard-free, byte-identical trees, on both lark-rs and the oracle —
  `recovery::op_guard_is_grammar_recoverable`.)*
* **`DEC_NUMBER` — recoverable as a parse error.** Without the guard, `0123` lexes as
  `DEC_NUMBER("0") DEC_NUMBER("123")` — two adjacent number atoms, which the Python
  grammar rejects at parse time. With the guard it is a *lex* error. The guard only
  relocates a guaranteed rejection from lex-time to parse-time; it does not change
  whether the input is accepted. **No engine.** *(Confirmed: `0123`/`007` rejected
  both ways, every accepted input byte-identical, on both lark-rs and the oracle —
  `recovery::dec_number_guard_is_grammar_recoverable`.)*

So after grammar-level recovery, **`STRING` is the sole bundled terminal that needs a
new engine primitive**, and that primitive is the simplest of the three Type-C shapes:
a fixed-position, fixed-width, all-or-nothing **leading** negative lookahead.

## Engine scope — a fixed-width boundary guard, not the Pike VM

The diagnosis answers the scoping question directly:

* **Sufficient primitive:** a **fixed-width boundary-assertion guard**. Match the
  lookaround-free core on the `regex` crate as today; attach a small descriptor
  `{ side: Start | End, polarity: Pos | Neg, set/literal, width }` checked against the
  bytes adjacent to the candidate match in `O(width)`. This is linear, joins no
  thread list, and re-enters no ε-closure. It covers `STRING` (the only bundled need)
  *and* the entire fixed-width-lookbehind class the census found in the wild (pep508
  `(?<====)`, ROS `(?<!_)\/`, the string idiom's `(?<!\\)`).
* **Worst case (if E4 declines to grammar-recover `DEC_NUMBER`):** add bounded
  backtracking of the immediately-preceding quantifier to satisfy a fixed-width
  *trailing* assertion. Still a narrow, fixed-width primitive.
* **What is *not* needed:** the general Pike VM of PR #110. Its thread-list machinery
  earns its keep only for **internal, variable-position, length-changing** assertions
  inside quantifiers — the `T3` tail of the strategy memo. Neither bundled grammar
  contains one, and the ~40-grammar census
  ([`LOOKAROUND_STRATEGY_ANALYSIS.md`](LOOKAROUND_STRATEGY_ANALYSIS.md) §10) found
  `T3` **empty**. Every real assertion is a fixed-width boundary.

This confirms the hypothesis the elimination plan already seeded ("the economical fix
is likely one narrow lexer-level bounded-lookahead guard … not the general Pike-VM
engine") and upgrades it from a guess to a proof for the entire bundled set.

## Recommended E4 shape

1. **Deploy the Type-A rewrites** (`LONG_STRING`, `REGEXP`, block-comment) — proven
   equivalent, zero risk; they rejoin the combined-DFA scan.
2. **Grammar-recover `OP` and `DEC_NUMBER`** — drop the guards. The recovery is
   already proven (the `recovery` tests + the oracle), so E4's only remaining work is
   to apply the edit to the bundled `lark.lark`/`python.lark` and fold the witnesses
   into the generated stdlib oracle.
3. **Add the fixed-width boundary guard for `STRING`** — a single leading
   `{ Start, Neg, "\"\"", width 2 }` descriptor wrapping its (reduced) `regex` core.
   Then `fancy-regex` can be removed (E4) and the bundled grammars become
   standalone/WASM-bakeable, with no Pike VM in the tree.

## Verification artifacts

* `tests/test_lookaround.rs::matchlen` — the six per-terminal proofs (three Type-A
  equivalences, three Type-C negative results).
* `tests/test_lookaround.rs::recovery` — the two grammar-level recovery proofs
  (`OP`, `DEC_NUMBER`): guarded ≡ guard-free trees in lark-rs, with the
  guard-sensitive witness exercised so the test is not vacuous. Independently
  confirmed on the Python Lark oracle.
* `tests/test_lookaround.rs::test_lookaround_oracle` — the cross-(parser×lexer)
  behavioral gate the eventual rewrites must keep green.
* `tests/test_stdlib.rs` — `STRING`'s end-to-end `""""` reject (E2a); the `OP`/
  `DEC_NUMBER` witnesses fold into the generated stdlib oracle when E4 applies the
  edits.
