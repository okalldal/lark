# ADR-0017: Oracle fidelity is for *intended* behavior, not implementation artifacts

- **Status:** Accepted (2026-06-18)
- **Date:** 2026-06-18

## Context

[ADR-0001](0001-python-lark-is-the-oracle.md) makes Python Lark the oracle, and
invariant §2.2 says expected trees/errors come from it. Read literally, that pulls
toward *byte-for-byte* parity with Python in every observable detail. But Python
Lark's observable behavior is a mix of two things:

- **Designed contract** — behavior a grammar author relies on and Lark documents or
  clearly intends (tree shapes, modifier semantics, the `re` dialect a grammar is
  authored against).
- **Circumstantial leakage** — behavior that merely falls out of *how* CPython-Lark
  happens to be implemented (an artifact of a particular data structure or traversal
  order), which no author should depend on.

Chasing leakage to the byte is how a rewrite drowns: it spends its budget matching
accidents instead of contracts, and forfeits the speed/maintainability that justify
the rewrite. We already make this trade implicitly and case-by-case —
[ADR-0004](0004-python-re-regex-dialect.md) matches Python's *meaning* for `\<`/`\>`
(a contract) while [ADR-0005](0005-lower-lookaround-into-the-dfa.md) /
`LOOKAROUND_SCOPE.md` deliberately refuse full backtracking lookaround (we decline to
reproduce an engine accident). What was missing was the *general rule* above those
instances. Two live `needs-decision` items forced it: #159 (we emit deduped `_ambig`
alternatives where Python repeats byte-identical ones) and #101 (our CYK accepts a
wholly-nullable transparent rule Python rejects).

## Decision

Oracle fidelity targets Python Lark's **intended behavior**, not its implementation
artifacts. For any observed divergence, route it on two axes:

| | **Cheap to match** | **Expensive to match** |
|---|---|---|
| **Intentional** (designed contract) | Match | **Match** — this is core fidelity; earn the cost |
| **Circumstantial** (impl leakage) | Match — harmless parity, keeps the diff small | **Diverge & document** (an ADR + a pinning test) |

Deciding *which axis a behavior sits on* — contract vs. leakage — is a §4
"written-rule + judgement" call: decide and record it (here, or in a follow-up ADR
the divergence cites), never escalate-by-default and never byte-chase-by-default.

**Corollary (more-permissive = unfalsifiable).** When lark-rs would *accept* input
the oracle *rejects*, we leave the oracle's domain entirely: there is no ground-truth
tree for an input Python won't parse, so the behavior is unfalsifiable. Absent a
deliberate, documented reason, **match the oracle's rejection** rather than be the
more permissive engine.

Worked examples that motivated this ADR:

- **#159 — keep our dedup.** Python's duplicate byte-identical `_ambig` children are
  leakage of `ForestToParseTree` (no dedup); matching is expensive and the output
  carries zero information → diverge & document, with a test that the dedup only
  collapses byte-identical trees.
- **#101 — match the rejection.** Python's `CYK doesn't support empty rules` is an
  intentional guard; our acceptance is an accidental ε-removal carve-out and triggers
  the corollary → reject, restoring parity.

## Consequences

- A reusable test for the long tail of "Python does X, should we?" questions — most
  resolve without escalation once the contract/leakage axis is named.
- We explicitly *decline* byte-for-byte parity where Python's behavior is an artifact
  and matching is costly. The cost is that "100% Python-identical, no exceptions" is
  no longer a claim we make (and never honestly was — see ADR-0004/0005); the public
  framing is "faithful to Lark's intended behavior, faster and more maintainable."
- Each accepted divergence still carries its own ADR + pinning test (§2.7,
  §6 DoD) — this ADR sets the *rule*, not a blanket licence to diverge.
- **Tripwire to revisit:** if a documented divergence is ever shown to break a
  *real-world* grammar (a wild-bank find, not a synthetic probe), the behavior was a
  contract after all — supersede the per-case ADR and match.
- Enforcement: per-divergence pinning tests (e.g. the #159 byte-identical-only guard,
  the #101 rejection pin); the compliance and wild banks remain the dominant
  correctness gate (§2.1–2.2).
