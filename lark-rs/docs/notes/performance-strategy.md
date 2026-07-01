# Performance strategy — synthesis & execution plan

- **Status:** Planning note (synthesis of existing decisions — **not** new policy, **not**
  an ADR). It restates and sequences choices already ratified in ADR-0011 / ADR-0027 /
  ADR-0029 and grounded in the BENCH.md profiling spike. An agent never self-ratifies
  policy or a public API (`PRINCIPLES.md` §6); the architect prioritizes and merges the
  escalate-tier slice (C7).
- **Date:** 2026-06-30
- **Sources:** ADR-0007 (deterministic counters), ADR-0011 (allocation-bound),
  ADR-0015 (`TreeBuilder` is the one shaping seam), ADR-0027 (semantic output direction),
  ADR-0029 (public API shape), `BENCH.md`, RFC `docs/notes/semantic-output-builder-design.md`,
  epic #225.

## TL;DR

The bottleneck is **already diagnosed, the approach already ratified, and ~80% of the
foundation already built.** This is an *execution* plan, not a fresh strategy: drive the
remaining critical path of epic #225, plus one independent default-path win.

**Critical path:** `C5 (#230) → C7 (#232) → C8 (#233)`.

## 1. Diagnosis (settled — ADR-0011)

Profiling (callgrind + DHAT, 92 KB JSON parse, BENCH.md "Profiling findings"):

- **~301K allocations / 105 MB churn per parse — ≈3 allocations per input byte.**
- ~40% of instructions in `memcpy` + `malloc`/`free`; ~10% in SipHash.
- Split: **~55% lexing** (regex-engine-dominated, *not* our logic) + **~32% reduce /
  tree-building** (`String` clones, per-node child `Vec`s).

Current measured speed vs Python Lark: ~6–7× LALR, ~13× Earley, ~27× CYK — real, but
short of the "10–100×" headline. **The gap is the `Tree`/`Token` representation, not the
parse algorithm.** A perf effort aimed at the algorithm is "aiming at the wrong 32%."

Concrete hotspots (confirmed in source):

- `Token` carries an owned `value: String` *and* a redundant `type_: String` — it already
  dispatches on `type_id: SymbolId` (`tree.rs`).
- Every SHIFT clones a `Token` onto the value stack (`parsers/lalr.rs`).
- Every reduction allocates `Tree::data: String` + a child `Vec` (`tree_builder.rs`).
- The lexer does a `self.names[&id].clone()` SipHash map lookup **per token**
  (`lexer/mod.rs`) — BENCH.md flags this as "pure waste, removable without the rework."

## 2. Approach (ratified — ADR-0027 / ADR-0029)

Don't mutate the default `Tree` (compatibility) and don't chase scattershot micro-opts.
The decided architecture is an internal `OutputBuilder` seam every engine reduces
through, with the generic tree as just *one* backend. The performance payoff is a
borrowed **`SpanTree<'i>`** backend: token values are `&'i input` spans (zero copy),
labels interned — eliminating the allocation half wholesale, behind a falsifiable gate
(materialize → byte-identical to the tree oracle, **plus** deterministic counters
`tree_nodes_built == 0`, `token_value_string_bytes == 0`).

## 3. You are here (epic #225)

| Slice | What | State |
|---|---|---|
| C1 #226 | Transformer value+trace oracle generator | ✅ closed |
| C2 #227 | `TreeBuilder` → `OutputBuilder` seam (no-behaviour-change) | ✅ closed |
| C3 #228 | `PythonTransformerOracleBuilder` (parity) | ✅ closed |
| C4 #229 | Token-callback / `Discard` / shaping parity pins | ✅ closed |
| C6 #231 | Public API shape decision | ✅ ADR-0029 |
| **C5 #230** | **Deterministic output-shape perf counters** | ✅ landed (#576) |
| **C7 #232** | **Public `parse_into<B>` surface** | ✅ landed (`f1a0b68`, PRs #579/#580) |
| **C8 #233** | **Zero-tree `SpanTree<'i>` *output* backend** | ✅ landed — `Lark::parse_span` behind the experimental `span-tree` feature; projection gate (span materialize == tree `parse()`, incl. whole compliance bank + non-ASCII pin) + counters (`tree_nodes_built == 0`, `token_value_string_bytes == 0`, one `semantic_reduce_call`/reduction) green (`tests/test_span_tree.rs`) |
| **C8.1 #582** | **Span-emitting lexer path (no `Token.value` alloc)** | ✅ landed — `parse_span` drives a value-less token source (`make_span_source`, both lexers); new `lexer_token_value_bytes` counter gated to `0` on the span path, `> 0` on owned `parse()` (LEXER vs OUTPUT alloc separately falsifiable, ADR-0007). Relative oracle + non-ASCII pin stay green; default `parse()` byte-identical. This is where the headline ADR-0011 allocation win lands. Bounded child-buffer reuse (C8.2 #583) still split out |

Deferred/gated correctly: C8b #242 (event stream) + C8c #243 (JSON tape) — each
`needs-decision`, blocked on naming a real consumer; C9 #234 (standalone); C10 #244
(bindings `OutputMode` taxonomy). Unrelated latent perf item: #568 (guard-body DFA
budget gate, `prio:later`).

## 4. The plan

### Track 1 — the headline lever (epic #225 critical path)

1. **C5 (#230) — output-shape counters. Start here.** Ready now, auto-tier, no blockers.
   Adds `tree_nodes_built`, `token_value_string_bytes`, `tree_label_string_bytes`,
   `child_vec_allocs`, `semantic_reduce_calls` to `src/perf.rs`. The "demonstrate-first"
   instrument (ADR-0007) that makes every later claim falsifiable and *is* the measuring
   stick C8's gate needs. Low risk, high leverage, unblocks the rest.
2. **C7 (#232) — public `parse_into<B>` seam.** API shape already ratified (ADR-0029):
   `parse_into<'i, B: OutputBuilder<'i>>(input, &mut builder)`, span-first trait,
   `is_discard` hook, LALR + basic/contextual only. **Escalate-tier** — the architect
   merges it (new public API). Gate that unblocks C8.
3. **C8 (#233) — `SpanTree<'i>`.** The zero-copy backend. Auto-tier *once the seam
   exists* (relative-oracle + counter-gated). Where the allocation half of ADR-0011's
   ~3 allocs/byte actually falls.

### Track 2 — default-`parse()` hygiene (independent)

The `SpanTree` win is **opt-in**; the default `parse()` stays allocation-bound by design.
The per-token name-clone + SipHash lookup (`lexer/mod.rs`) is a separable auto-tier win
that helps *every* parse regardless of backend, and the C5 counters gate it cleanly. Per
"out-of-scope-find → **file an issue**, never silently fix," this becomes its own issue.

### Out of scope (so we don't chase the wrong 32%)

- SIMD lexing — ADR-0027 non-goal (attacks the lexer half; its own ADR if ever funded).
- "simdjson-class for arbitrary grammars" claims.
- Any algorithm rewrite (the engines are already near-optimal in complexity class, with
  scaling gates proving it).
- Wall-clock gates — ADR-0007: deterministic counters only; wall-clock stays a trend.

## 5. The one open decision (architect's call)

Everything above is execution of a ratified plan. The single open product fork: should
the **default `Tree`** itself get cheaper (e.g. interned / `Box<str>` labels — oracle-safe
but a public-surface reshape), or do we accept "default = compatible/owned, opt-in
`SpanTree` = fast" as the permanent story (what ADR-0029 currently commits to)? Not an
agent decision to guess.

## 6. Recommended sequencing

`C5` (now, auto) → `C7` (escalate, architect merge) → `C8` (auto, the win) → record the
trend in BENCH.md. File the Track-2 default-path issue alongside C5 so its counter gate
rides the same instrumentation.
