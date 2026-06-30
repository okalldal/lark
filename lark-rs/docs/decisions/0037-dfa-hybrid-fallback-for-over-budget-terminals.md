# ADR-0037: Hybrid-DFA fallback for over-budget terminals (the `.*a.{N}` pathology)

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-30

## Context

The default DFA lexer backend (`lexer/dfa.rs`) builds the combined scanner by
compiling each plain/guarded base to a Thompson NFA and **eagerly, fully
determinizing** it to a `regex-automata` `dense::DFA` — historically with no
`dfa_size_limit`. A terminal whose *minimal* DFA is exponential in its source —
the classic `.*a.{N}` family `T: /[01]*1[01]{N}/` — blows the determinizer to
`2^(N+1)` states and hangs unbounded. Python `re` matches the same terminal in
linear time (it never determinizes), so this is a pure resource pathology, not a
behavioral divergence: **both engines accept** the inputs that build (issue #349,
catalog H4-12).

The deterministic `dense_build_bytes` work counter (`--features perf-counters`)
measured the blow-up: N=4 → 5184 B, N=10 → 311616 B (≈60×, roughly doubling per
+1 in N). The XFAIL `h4_12_dense_dfa_build_is_subexponential` pinned it.

Issue #349 framed two contracts:

1. **Hybrid fallback** — for over-budget terminals, fall back to the lazy/hybrid
   DFA (the same engine the start-byte prefilter and the `regex` scanner backend
   already use), so the grammar still builds and lexes. The hybrid DFA realizes
   states on demand, so its build cost is flat; its matches are byte-identical to
   the dense DFA's, so oracle parity is preserved.
2. **Size-limit refusal** — set a `dfa_size_limit` and raise a categorized
   build-time `GrammarError` when a terminal exceeds it.

## Decision

Adopt **contract 1 (hybrid fallback)**. The engine builder
(`build_partitioned_dfa`) probes each source under a per-source `dfa_size_limit`
(`DENSE_PER_SOURCE_BUDGET` = 64 KiB); a source that overflows is routed to a
parallel **lazy/hybrid** sub-engine (`CombinedDfa::Hybrid`) instead of the dense
union, while every in-budget source stays on the eager dense engine. `match_at`
(leftmost-first) and `guarded_best` (all-matches overlapping) consult both
sub-engines and re-merge winners by `(rank, branch_order)` — the global
leftmost-first order — so the partition never changes which terminal wins.

The fallback is gated on a genuine **size overflow**: only a dense build that fails
`is_size_limit_exceeded()` (the DFA/determinize size limits and the too-many-states
overflows) is rerouted to hybrid. Any *other* dense error — e.g. a Unicode word
boundary the dense determinizer does not support — is a real build error and
surfaces as an `InvalidRegex` attributed to that exact source, never silently
rerouted (which would otherwise misattribute the message, or accept a pattern the
eager path rejected). Contract 2 thus also survives as an **inner guard**: a source
the hybrid engine itself cannot build still surfaces the categorized `GrammarError`.

**Why 1 over 2.** Refusing (contract 2) would make lark-rs *reject a grammar
Python accepts* — a less-permissive divergence with no behavioral justification
(the terminal is perfectly valid; only lark-rs's chosen engine struggles with it).
The project invariant is oracle parity (Python Lark is the oracle), and ADR-0017's
corollary routes a divergence by *intentional contract vs. circumstantial leakage*
× *cheap vs. expensive*: this is **circumstantial** (an artifact of choosing eager
determinization) and **cheap to match** (the hybrid engine already exists and is
byte-identical), so the rule says match, not diverge. The hybrid fallback keeps the
grammar building and lexing exactly as Python does, with `dense_build_bytes` flat
per source.

## Consequences

- **Buys:** the `.*a.{N}` family (and any future eager-determinization blow-up)
  builds and lexes in bounded work; `dense_build_bytes` stays flat across N. Oracle
  parity is preserved — no grammar that built before regresses, and no grammar
  Python accepts is now refused.
- **Costs:** an over-budget terminal lexes via the lazy DFA, which is marginally
  slower per token than a fully-realized dense DFA (states are built on first use,
  then cached). This is paid only by genuinely pathological terminals — every
  bundled/well-behaved terminal (≤ ~13 KiB dense, measured) stays on the eager
  engine. The lazy DFA needs a mutable scratch cache; `Lark` is already `!Sync`
  (the `regex` backend holds a `RefCell` too), so the per-engine `RefCell<Cache>`
  is consistent.
- **Rules out:** rejecting these terminals (contract 2 as the *primary* path).
- **Tripwire:** `h4_12_dense_dfa_build_is_subexponential` and the
  `counted-repeat` sweep in `test_lexer_dfa_build_scaling.rs` assert
  `dense_build_bytes` stays flat across N (both `--features perf-counters`). The
  64 KiB budget sits ~5× above the legitimate per-terminal ceiling and well below
  where the `.*a.{N}` family lands by N≈10; if a real grammar ever legitimately
  needs a larger single-terminal dense DFA it will silently (but correctly) fall
  back to hybrid — raise the budget if that shows up as a measured throughput
  regression.

Enforced by `lexer/dfa.rs::build_partitioned_dfa` +
`tests/test_bounty_findings_h4.rs::h4_12_dense_dfa_build_is_subexponential` +
`tests/test_lexer_dfa_build_scaling.rs` (counted-repeat sweep).
