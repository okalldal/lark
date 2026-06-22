# Lookaround Scope — the two-category contract

Since **L4** (`LEXER_DFA_PLAN.md`) the lexer has **no fallback regex engine**: every
terminal either compiles on the linear `regex` crate, **lowers** into the
`regex-automata` DFA through an audited path, or **fails the grammar build** with a
categorized `GrammarError::LookaroundScope`. This document is the scoreboard's prose
twin: it says, for every refused shape, *which* of two very different things the
refusal means. The machine-checked source of truth is
[`tests/test_lookaround_scope.rs`](../tests/test_lookaround_scope.rs) — its
exhaustiveness meta-test forces every refusal variant in the code
(`classify::Rejection`, `classify::DeclineReason`) to map to a row here or carry an
explicit defensive justification.

The boundary principle: **supported = an audited lowering exists.** The rejected set
is kept a subset of what Python Lark accepts wherever possible, and every deliberate
parity break is named below.

## What lowers (for contrast)

Leading/trailing boundary lookahead of bounded width (M1/M2), fixed-offset bounded
lookbehind (M3), the audited **delimited-token idioms** — `python.STRING` (M4),
`lark.REGEXP`, `python.LONG_STRING` (Stage B), and the **short-string** idiom
`<q>.+?(?<!\\)(\\\\)*?<q>` (idiom #4, the wild-bank dotmotif `FLEXIBLE_KEY` — a
single-char-delimited token with a non-empty lazy escaped body, lowered by the
escape-pair normalization with a pure-pair-run/transition-item split; see the
section comment in `src/lookaround/lower.rs`) — and guarded bases proven
*leftmost-first ≡ longest* by the semantic realizability gate
(`lower.rs::is_leftmost_longest`, the exact product-DFA decision that admits e.g.
`python.DEC_NUMBER`'s `0(?:_?0)*` arm). Every bundled lookaround terminal lowers; the
bundled grammars build with zero refusals.

Two additions from the wild-bank fence family (2026-06):

* **Leading boundary lookahead of *unbounded* width** — `(?!\[=*\[)BODY` (the
  gersemi UNQUOTED_ELEMENT guard). A leading guard runs anchored at the match
  start and never affects the accept position, so the bounded-width demand was
  unnecessary at that one position. (Cost caveat: the per-attempt guard run is
  then bounded only by the remaining input — the same worst case Python `re`
  pays — so such a terminal is not linear-by-construction; see
  `lexer/guard.rs::Guard::holds`.) Trailing/internal unbounded stay NYI below.
* **The fence idiom (idiom #5)** — `OPEN(?P<tag>T)SEP body (?P=tag)CLOSE`, the
  tag-echo family (HCL2 heredocs, CMake bracket arguments, Lua long brackets,
  PostgreSQL dollar-quoting). Non-regular, so it does NOT lower into the DFA;
  it is recognized exactly (`lower.rs::recognize_fence_idiom` — literal
  open/sep/close sections, a universal lazy body, one backref) and matched by
  the two-phase linear-per-attempt `lexer/fence.rs::FenceMatcher`. The
  recognizer rejects greedy or content-constrained bodies rather than risk a
  silent divergence from Python's lazy-backref semantics, and honors the
  body's minimum (`+?` ≥ 1 char: `[[]]` is a lex error, exactly as in Python).

Positional analysis runs after the **vacuous-group splice**
(`classify.rs::unwrap_vacuous_groups`): a bare unquantified `(?:…)` is spliced into
its enclosing concatenation (`(?:X) ≡ X` exactly), so a boundary guard the loader's
terminal-*reference* composition buried inside a wrapper — the wild-bank mappyfile
`SIGNED_INT: ["-"|"+"] INT` shape, composed as `(?:\-|\+)?(?:[0-9]+(?![_a-zA-Z]))` —
classifies as the trailing boundary it is and lowers via M1. Quantified, capturing,
and flag-scoped groups stay opaque.

## Category 1 — OutOfScope (by-design non-goals)

End-to-end tests **assert these rejections as the contract**. They will not be
lowered in any future version; changing that requires a documented scope decision
here first.

| Shape | Example | Variant | Why it is a non-goal |
|---|---|---|---|
| Internal (mid-pattern) lookahead | `a(?=b)c`, the block-comment `(\*(?!\/)\|[^*])*` | `Rejection::Internal` | Priority-entangled: a mid-pattern assertion couples greedy/lazy match length to positions a per-state guard cannot represent; a general lowering means product-construction state blowup and an audit surface this project deliberately refuses. **Named parity break** (Python's backtracking engine accepts these). The **audited delimited-token idioms are the sanctioned growth path**: a common, exactly-recognizable shape can be admitted one Stage-B audit at a time (STRING/REGEXP/LONG_STRING are the precedents). |
| Variable-width lookbehind body | `(?<!a*)b` | `Rejection::VariableWidthBehind` (+ defensive `UnboundedLookbehindBody`) | **Python `re` rejects these too** ("look-behind requires fixed-width pattern") — rejection is oracle *parity*, not a break. |
| Backreferences | `(a)\1b`, `(a)(?=\1)`, `(?P<x>a)(?P=x)` | `DeclineReason::BacktrackingOnlySyntax`, `Rejection::Backref` | Not a regular language; no DFA can host it. **The named parity break class** (with the rest of backtracking-only syntax: atomic groups, possessive quantifiers). No bundled grammar uses them. Every spelling refuses with the *same* categorized message: the escape forms `\1`/`\k<n>`/`\g{1}` and the Python named form `(?P=name)` (N4 — the front-end keeps the latter verbatim so it routes through `BacktrackingOnlySyntax` like the rest, rather than leaking a raw regex error). **One audited exception:** the exact tag-echo **fence idiom** (`(?P<tag>…)…(?P=tag)`, see "What lowers") is recognized and matched by its own linear two-phase scanner — general backreferences remain out of scope. |
| Nested assertions | `(?=(?!a)b)c` | `Rejection::Nested` | Audit cost out of proportion to demand; flatten the assertion instead. |
| Quantified assertions | `a(?=b)?` | `Rejection::QuantifiedAssertion` (+ defensive `QuantifiedLookbehind`) | Degenerate and priority-entangled; almost always a bug in the grammar. |
| Zero-width degenerates | `(?!a)` alone, `a(?<=())b` | `DeclineReason::ZeroWidthBranch`, `ZeroWidthLookbehindBody` | A zero-width terminal/window; the lexer forbids zero-width matches. |

## Category 2 — NotYetImplemented (conservative rejections)

In-principle lowerable; rejected **cleanly** today so they can never silently
mis-lex. The scoreboard rows are **promotion tripwires**: if one of these starts
building, the test fails loudly and demands the promotion protocol below.

| Shape | Example | Variant | Path to support |
|---|---|---|---|
| Fixed-width lookbehind at variable offset | `\w+(?<!_)q` | `DeclineReason::VariableOffsetLookbehind` | Python accepts these (the body is fixed-width). Generalize M3's offset model (window-carrying over variable prefixes) or admit common shapes as idioms. The headline NYI case. |
| Unbounded trailing lookahead | `[a-z]+(?=ab+)` | `Rejection::Unbounded` | Regular (classic lex trailing context); needs a reverse-scan/product mechanism. No current plan — demand-driven. (The *leading* unbounded case is now supported — see "What lowers".) |
| Non-realizable guarded base | `(ab\|abc)(?!z)`, `ab??(?!c)` | `DeclineReason::NonRealizableGuardedBase` | The base prefers a shorter match than its longest, so the longest-accept accumulator cannot host it. The semantic gate already proves the provable cases; widening further means a preference-aware accumulator. |
| Assertion in an interior group | `(a(?<!b))c` | `DeclineReason::NestedInGroup` | Needs group-aware peeling for **capturing/flag** groups. (A bare unquantified `(?:…)` is no longer this case at all: the vacuous-group splice normalizes it away everywhere — a proven identity, `(?:X) ≡ X` — which is how the wild mappyfile composition shape lowers.) |
| VERBOSE-mode lookaround | `(?x:[0-9]+ (?![0-9]))`, or any lookaround pattern under `g_regex_flags = VERBOSE` | `DeclineReason::VerboseMode` | The analyzer's width/offset arithmetic is not verbose-aware; under `x` (whether from a whole-pattern wrapper or the global flag) whitespace/comments would be miscounted as literal width (a false-accept hazard). Needs a verbose-aware frontend. |
| Analyzer parse gaps | — | `DeclineReason::FrontendParse` | Defensive catch-all (terminal loading gates on the same parser, so it is unreachable end-to-end today). Any instance found in the wild is a frontend bug to fix. |

## The promotion protocol (NYI → supported)

A pattern leaves Category 2 only through the **Stage-B audit ladder**
(`LEXER_DFA_PLAN.md`): an exact recognizer or a *proven* gate extension; a generative
equivalence sweep against the `fancy-regex` dev-oracle over the shape's load-bearing
alphabet; a mutation canary proving the net catches a wrong lowering; route-level
pins; and a scanner-differential population entry. Then move the scoreboard row to a
`*_lowers` pin and update this document. Precedents: the M4 STRING splice, the
Stage-B REGEXP/LONG_STRING idioms, the `is_leftmost_longest` semantic-gate widening
that admitted `python.DEC_NUMBER`.

Moving something *out of Category 1* is a scope decision, not an implementation task:
amend this document (and the recorded rationale) first, then treat it as Category 2.

## Where `fancy-regex` stands

`fancy-regex` is **not a runtime dependency and not a user escape hatch**. It remains
a dev-dependency as the independent per-pattern oracle (equivalence/proof tests), and
the `fancy-oracle` cargo feature (default **off**, CI/test-only) resurrects the
historical fancy side-probes of the `Regex` reference backend so the whole-lexer
differential keeps an independent reference. Default builds contain zero fancy-regex
code, and grammar-build outcomes are identical with and without the feature **by
construction**: the feature build routes every regex-rejected terminal through the
same refusal seam first, and only a terminal that *lowers* gets a fancy reference
probe — the feature swaps matchers, never the accepted grammar set.
