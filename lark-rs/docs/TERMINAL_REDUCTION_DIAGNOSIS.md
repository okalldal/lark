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

**Answer: yes, comfortably.** The irreducible surface collapses to a single *shape* —
a *fixed-width* boundary assertion (three terminals: `STRING`, `OP`, `DEC_NUMBER`).
None of it needs the general thread-list/ε-closure Pike VM. Grammar-level recovery
would shrink the *bundled* need to `STRING` alone, but — the adversarial finding
below — that recovery breaks under `%import`, so the primitive is needed for all three
to stay faithful to Python Lark. It is still a narrow, fixed-width primitive.

## Method

Each terminal is classified at two levels:

1. **Terminal level (decidable, oracle-free).** Run the original lookaround pattern
   on `fancy-regex` and a candidate lookaround-free rewrite on the `regex` crate, and
   compare the two anchored matched-prefix functions over an exhaustive corpus. This
   is the existing E2a `matchlen` harness (`tests/test_lookaround.rs`), now extended
   to `OP`/`REGEXP`/`DEC_NUMBER`. Both engines are leftmost-first/backtracking like
   CPython `re`, so the comparison is faithful and the result is a *proof*, not a
   sample.
2. **Grammar level (confirmed end-to-end, *context-dependent*).** Even when a guard
   is terminal-level irreducible, dropping it may not change a *particular grammar's*
   accept/reject — if the alternative tokenization is itself a parse error or is
   resolved by maximal munch. In the bundled grammars this holds for `OP`/`DEC_NUMBER`
   (proven: guard removed → byte-identical trees, on both lark-rs and the oracle).
   **But it is a property of the context, not the terminal**, and it breaks when the
   terminal is `%import`ed into a non-recovering grammar (proven: the import witnesses
   below diverge). `STRING` is irreducible in *every* context. So the honest grammar-
   level conclusion is narrower than "recoverable" — see the recovery section.

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

## Grammar-level recovery — real, but context-local (the adversarial caveat)

Terminal-level irreducibility does not imply the *grammar* needs the guard. In the
**bundled grammars' own context**, dropping a Type-C guard changes nothing:

* **`STRING` — genuinely irreducible, even in context.** The alternative reading of
  `""""` is `STRING STRING` (two empty strings), which is *valid* to the parser, so
  the guard is the only thing that rejects `""""`. **Needs the primitive.** (E2a.)
* **`OP` — recovered by maximal munch.** `?foo` lexes as the longer `RULE` token, so
  in `lark.lark` the guard is redundant. *(Confirmed: guarded ≡ guard-free, identical
  trees, lark-rs + oracle — `recovery::op_guard_is_grammar_recoverable`.)*
* **`DEC_NUMBER` — recovered as a parse error.** In `python.lark` no production
  juxtaposes two numbers, so `0123` is a parse error with or without the guard.
  *(Confirmed — `recovery::dec_number_guard_is_grammar_recoverable`.)*

**But recovery is a property of the importing context, not the terminal — and it
breaks under `%import`.** These terminals are importable, and a user grammar can
supply a context with no recovering layer. Proven (lark-rs *and* the oracle agree),
`recovery::recovery_fails_under_adversarial_import`:

| Import context | Witness | Guarded (Python/today) | Guard removed | Verdict |
|---|---|---|---|---|
| `start: NUMBER+` (numbers can juxtapose) | `0123`, `001`, `007` | **reject** | **accept** | diverges |
| `OP` beside `NAME: /[a-z]+/` (nothing absorbs `?foo`) | `?a`, `?foo` | **reject** | **accept** | diverges |

In `start: NUMBER+`, guard-free reads `0123` as `0`,`123` and accepts; with `OP`
beside a plain `NAME`, guard-free reads `?foo` as `OP("?") NAME("foo")` and accepts —
both diverging from Python Lark, which keeps the guard regardless of import context.

**Consequence: the "drop the guard" shortcut is unsafe.** Deleting these guards from
the bundled grammars would mis-parse any grammar that imports them into a
non-recovering context. To stay oracle-faithful, lark-rs must **preserve** the
guards' match functions for `OP` and `DEC_NUMBER` too — not just `STRING`. The
recovery result is still useful (it explains why the bundled grammars work and bounds
the blast radius), but it is **not** a substitute for the primitive.

So the count is: all **three** Type-C terminals need the primitive for import-safety
— but all three are still fixed-width boundary assertions, so the same narrow guard
covers them. The Pike VM is still not needed.

## Engine scope — a fixed-width boundary guard, not the Pike VM

The diagnosis answers the scoping question directly:

* **Sufficient primitive:** a **fixed-width boundary-assertion guard**. Match the
  lookaround-free core on the `regex` crate as today; attach a small descriptor
  `{ side: Start | End, polarity: Pos | Neg, set/literal, width }` checked against the
  bytes adjacent to the candidate match in `O(width)`. This is linear, joins no
  thread list, and re-enters no ε-closure. It covers all three import-unsafe Type-C
  terminals — `STRING` (leading, all-or-nothing), `OP` (trailing, all-or-nothing) —
  *and* the entire fixed-width-lookbehind class the census found in the wild (pep508
  `(?<====)`, ROS `(?<!_)\/`, the string idiom's `(?<!\\)`).
* **`DEC_NUMBER`'s trailing guard** additionally needs **bounded backtracking of the
  immediately-preceding quantifier** (its `(?![1-9])` is length-changing — see above),
  so it matches the run, checks the assertion, and shrinks the run by one if it fails.
  Still a single fixed-width trailing assertion on one quantifier — a narrow primitive,
  not the Pike VM.
* **What is *not* needed:** the general Pike VM of PR #110. Its thread-list machinery
  earns its keep only for **internal, variable-position, length-changing** assertions
  inside quantifiers — the `T3` tail of the strategy memo. Neither bundled grammar
  contains one, and the ~40-grammar census
  ([`LOOKAROUND_STRATEGY_ANALYSIS.md`](LOOKAROUND_STRATEGY_ANALYSIS.md) §10) found
  `T3` **empty**. Every real assertion is a fixed-width boundary.

This confirms the hypothesis the elimination plan already seeded ("the economical fix
is likely one narrow lexer-level bounded-lookahead guard … not the general Pike-VM
engine") and upgrades it from a guess to a proof for the entire bundled set.

## Adversarial hardening (what was tried to break this)

* **Type-A equivalences fuzzed.** Beyond the exhaustive short-string proofs, an
  exploratory sweep ran the originals (`fancy-regex`) against the rewrites (`regex`)
  over millions of randomized strings biased toward the seams (escaped delimiters,
  odd backslash runs, `**/` tails, flag suffixes, lazy-vs-greedy ends). **No
  counterexample** to `LONG_STRING`, `REGEXP`, or the block comment. (The fuzzer
  itself is not committed — the committed exhaustive `matchlen` proofs are the gate.)
* **Recovery attacked via `%import`.** The decisive negative result above:
  grammar-level recovery is context-local and breaks when the terminal is imported
  into a non-recovering grammar — committed as
  `recovery::recovery_fails_under_adversarial_import`.

## Recommended E4 shape

1. **Deploy the Type-A rewrites** (`LONG_STRING`, `REGEXP`, block-comment) — proven
   equivalent (and fuzz-hardened), zero risk; they rejoin the combined-DFA scan.
2. **Route `STRING`, `OP`, `DEC_NUMBER` through the fixed-width boundary guard** —
   **do not delete** their guards (that breaks imports, per the table above).
   `STRING` is a leading `{ Start, Neg, "\"\"", width 2 }`; `OP` a trailing
   `{ End, Neg, [a-z], width 1 }`; `DEC_NUMBER` a trailing `{ End, Neg, [1-9], width 1 }`
   with single-quantifier backtracking. Each wraps its (reduced) `regex` core.
3. **Remove `fancy-regex`** once the primitive lands — the bundled `python`/`lark`
   grammars then bake into standalone/WASM, with no Pike VM in the tree.

## Verification artifacts

* `tests/test_lookaround.rs::matchlen` — the six per-terminal proofs (three Type-A
  equivalences, three Type-C negative results).
* `tests/test_lookaround.rs::recovery` — the in-context recovery proofs (`OP`,
  `DEC_NUMBER`: guarded ≡ guard-free trees, guard-sensitive witness exercised) **and**
  `recovery_fails_under_adversarial_import`, which pins that recovery breaks under a
  non-recovering import. All triangulated against the Python Lark oracle.
* `tests/test_lookaround.rs::test_lookaround_oracle` — the cross-(parser×lexer)
  behavioral gate the eventual rewrites must keep green.
* `tests/test_stdlib.rs` — `STRING`'s end-to-end `""""` reject (E2a); the `OP`/
  `DEC_NUMBER` import witnesses fold into the generated stdlib oracle when E4 lands
  the primitive.
