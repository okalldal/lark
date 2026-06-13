# ADR-0010: Lookaround strategy history — three approaches we abandoned before the DFA

- **Status:** Accepted (records superseded approaches; complements [ADR-0005](0005-lower-lookaround-into-the-dfa.md))
- **Date:** backfilled 2026-06-13 from PRs #88, #110, #111, #112, #113, #115, #137

## Context

[ADR-0005](0005-lower-lookaround-into-the-dfa.md) records the *endpoint* — lower
bounded lookaround into the DFA, no backtracking runtime engine — but reads as if
that were the plan all along. It wasn't. The strategy reversed **three times**,
twice *after* code or grammar edits had already landed. The "why we didn't keep
approach X" reasoning is the expensive knowledge, and it lived only in the PR
bodies. This ADR preserves it so the dead ends aren't re-explored.

## The four legs

**Leg 1 — fancy-regex runtime overlay (#88, shipped then removed).** First
solution: send a terminal to `fancy-regex` only when the `regex` crate rejects
its pattern. Abandoned (#110) as *"a correctness/distribution liability: a real
ReDoS in `lark.REGEXP`, a possible wrong-answer on backtrack-limit, and patterns
that can't be baked into the standalone/WASM/C runtimes."*

**Leg 2 — Pike-VM lowering engine (#110, closed not merged).** Replace
backtracking with *"a linear, backtracking-free Pike-VM lowering engine."* #110
also disproved its own central premise by re-scanning the grammars: *"The plan
assumed all bundled assertions sit at token boundaries; in fact only `DEC_NUMBER`
and `OP` do … the 'boundary peek' path was not complete."* PR closed, engine
shelved.

**Leg 3 — pure elimination, no engine at all (#111), then disproved (#112).**
Superseded #110: *"Decision: pure elimination (Option 1b), no runtime engine …
elimination rejoins the fast combined-DFA scan, carries zero `re`-parity
maintenance surface, and makes the bundled grammars standalone/WASM-bakeable for
free."* But E2a (#112) falsified its premise — not every terminal is rewritable
lookaround-free: *"`python.STRING` ⛔ Irreducible — proven negative result …
`(?!"")` makes `""""` a lex error while `"" ""` is valid … no lookaround-free
regex, priority, or grammar-level fix can reproduce it."* (#112's earlier
revisions that rewrote `LONG_STRING`/`STRING` grammars were reverted.)

**Leg 4 — combined DFA over lowered terminals (#113, #115, final).** Re-aimed to
*"a combined DFA lexer — a DFA, not PR #110's Pike-VM."* The distinction that
defuses #111's anti-engine memo: *"a DFA over lowered, lookaround-free terminals
executes no lookaround, has no CPython-`re`-parity surface, and is faster than a
Pike-VM — so the strategy memo's anti-engine arguments (which targeted a
match-time lookaround engine) don't apply."* #115 then dissolved the
reducible/irreducible terminal tiers: at the automaton level the distinction
*"dissolves … the old 'edit the Tier-E grammars' phase is dropped."*

## Consequences / lessons preserved

- **Don't reintroduce a runtime lookaround engine or grammar rewrites.** Both
  were tried; the recorded reasons they failed (re-parity surface, ReDoS,
  un-bakeable patterns, `python.STRING` irreducibility) still hold.
- **fancy-regex was once a runtime dependency.** #137 (L4) removed it but kept it
  deliberately as a *test-only* differential oracle (`fancy-oracle` feature) —
  see ADR-0005's consequences.
- The lesson generalizes: re-scan the actual grammars before committing to a plan
  whose premise is "all the inputs look like X." Two of these legs died on that.
