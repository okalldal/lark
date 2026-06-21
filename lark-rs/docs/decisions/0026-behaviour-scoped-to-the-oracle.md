# ADR-0026: Behaviour is scoped to the Python Lark oracle — beyond-oracle behaviour is escalate + needs a validation story

- **Status:** Accepted (2026-06-21; architect ratified)
- **Date:** 2026-06-21

> Forward-looking *policy* ADR (architect-directed, 2026-06-21). It resolves the
> `needs-decision` blocker #211 (epic #209, editor/LSP-grade recovery) at the
> policy level and adds a `PRINCIPLES.md` §3 default. There is no oracle for "how
> far past Python should our behaviour go?" — that is the architect's to set.

## Context

The shipped behaviour of lark-rs is grounded by one fact (§2.1–2.2, ADR-0001):
**Python Lark is the oracle.** A behaviour we can run Python against is
falsifiable; an agent can build it unattended and self-check. This is what made
single-token-deletion recovery (#43/#94) `good-autonomous` — it is byte-for-byte
what Python's `on_error=lambda e: True` does.

The boundary shows clearly in the recovery feature set. Some recovery behaviors
have a Python counterpart (interactive parser API: `InteractiveParser` with
`accepts()`/`feed_token`/`resume_parse`); others do not (automatic richer
strategies like token insertion, inline `ERROR` nodes in the tree). The first
class is inside the oracle and can be built autonomously; the second has no
ground truth — what is the falsifiable acceptance basis for behaviour Python
doesn't have?

Absent a written rule, an agent either guesses (unsafe) or stalls. "How far past
the oracle do we go?" is product direction, not something a test settles.

## Decision

**lark-rs's behaviour is scoped to Python Lark's behaviour.** Ship behaviour you
can falsify against the oracle. Behaviour with **no Python counterpart** is **not**
an autonomous default: it is **escalate-tier** and requires *both*

1. a concrete consumer demand (not a hypothetical audience), and
2. an architect-approved **validation story** that substitutes for the missing
   oracle — in falsifiability order: a *partial oracle* (the Python-shared subset),
   a *relative oracle* (re-ground against an existing oracle-backed path, e.g. a
   projection invariant), property tests, and only then curated goldens for the
   irreducible residue. "Curated goldens for everything" is not acceptable (§2.7).

This is the §2.2 corollary generalised from *permissiveness* to *scope*: being
behaviourally **richer** than the oracle is as unfalsifiable as being more
**permissive** than it. ADR-0017 routes divergences on behaviour Python *also*
has; this routes **new** behaviour by whether Python grounds it at all.

### Worked example: recovery features

- **Interactive parser API (#168) — oracle-backed.** Python has `InteractiveParser`;
  port the oracle-backed subset (no new parser *behaviour* beyond Python), validated
  by step-granular differentials + relative-oracle property tests (resume == parse,
  exhaust+eof == parse, `accepts()` honesty). Rust may expose convenience spellings
  that lower directly to a Python-backed operation, but must add no operation that
  does something Python's parser cannot.
- **Automatic richer strategies (#164) — beyond-oracle, deferred.** No Python
  counterpart, no concrete consumer demand. Revisit only on a consumer + a
  validation story. The interactive parser surface largely subsumes this (callers
  can do their own insertion/resync through the oracle-backed API).
- **Inline `ERROR` nodes (#165) — beyond-oracle, deferred.** If demand arrives, the
  validation story is the *projection invariant* (strip in-tree error decorations ⇒
  byte-identical to the oracle-backed "alongside" tree, over the whole recovery
  bank) + curated goldens for marker placement only.

## Consequences

- **Resolves the "how far past Python?" ambiguity.** Each beyond-oracle feature
  has a written, falsifiable done-when (or a deferral reason with a named
  validation story for when demand arrives).
- **No unfalsifiable behaviour ships unattended.** The default answer to "should
  we build behaviour Python lacks?" is "escalate," not "guess."
- **It is a gate, not a ban.** Exceeding Python is *allowed* — it just requires the
  architect + a named validation story, never an autonomous default. The project
  can still lead past Python deliberately.
- **Pairs with ADR-0025.** That ADR frees the *API surface* (breaking is free,
  pre-users); this one disciplines *behavioural scope* (stay oracle-grounded
  unless the architect funds a substitute gate). Surface is cheap to change;
  behaviour must stay checkable.
- **Merge tiers unchanged (§6).** Beyond-oracle behaviour is escalate because it
  is ungated product direction (§4 lens), independent of ADR-0025.
- **Tripwire / revisit.** If a class of beyond-oracle behaviour (e.g. inline error
  nodes) gets a *standing* validation gate good enough to make it routinely
  self-checkable, promote that into §3 so it stops being escalate-by-default —
  the same gate-building loop §0/§8 describes. Enforced by `PRINCIPLES.md` §3 +
  the §6 merge-tier review.
