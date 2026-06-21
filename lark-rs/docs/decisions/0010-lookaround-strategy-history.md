# ADR-0010: Lookaround strategy history — three approaches we abandoned before the DFA

- **Status:** Accepted (records superseded approaches; complements [ADR-0005](0005-lower-lookaround-into-the-dfa.md))
- **Date:** backfilled 2026-06-13 from PRs #88, #110, #111, #112, #113, #115, #137

## Context

[ADR-0005](0005-lower-lookaround-into-the-dfa.md) records the *endpoint* — lower
bounded lookaround into the DFA, no backtracking runtime engine — but the strategy
reversed **three times**, twice *after* code or grammar edits had already landed.
The reasons each approach was abandoned are load-bearing: they constrain future
design so the dead ends aren't re-explored.

## The four legs

**Leg 1 — fancy-regex runtime overlay (#88).** Send a terminal to `fancy-regex`
only when the `regex` crate rejects its pattern. Abandoned: a correctness and
distribution liability (ReDoS in `lark.REGEXP`, possible wrong-answer on
backtrack-limit, and patterns that can't be baked into standalone/WASM/C runtimes).

**Leg 2 — Pike-VM lowering engine (#110).** A linear, backtracking-free Pike-VM.
Its central premise — all bundled assertions sit at token boundaries — was
disproved by re-scanning the grammars: only `DEC_NUMBER` and `OP` do. The
"boundary peek" path was incomplete.

**Leg 3 — pure elimination, no engine at all (#111, disproved by #112).** Rewrite
every terminal to be lookaround-free. Falsified: `python.STRING` is irreducible —
`(?!"")` makes `""""` a lex error while `"" ""` is valid; no lookaround-free
regex, priority, or grammar-level fix can reproduce it.

**Leg 4 — combined DFA over lowered terminals (#113, #115, adopted).** A DFA
over lowered, lookaround-free terminals executes no lookaround, has no
CPython-`re`-parity surface, and is faster than a Pike-VM — so Leg 3's anti-engine
arguments (which targeted a *match-time lookaround* engine, not an automaton over
already-lowered terminals) don't apply. At the
automaton level the reducible/irreducible terminal distinction dissolves.

## Consequences / lessons preserved

- **Don't reintroduce a runtime lookaround engine or grammar rewrites.** Both
  were tried; the recorded reasons they failed (re-parity surface, ReDoS,
  un-bakeable patterns, `python.STRING` irreducibility) still hold.
- **fancy-regex was once a runtime dependency.** #137 (L4) removed it but kept it
  deliberately as a *test-only* differential oracle (`fancy-oracle` feature) —
  see ADR-0005's consequences.
- The lesson generalizes: re-scan the actual grammars before committing to a plan
  whose premise is "all the inputs look like X." Two of these legs died on that.
