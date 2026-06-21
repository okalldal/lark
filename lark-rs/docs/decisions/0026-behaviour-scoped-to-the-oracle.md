# ADR-0026: Behaviour is scoped to the Python Lark oracle — beyond-oracle behaviour is escalate + needs a validation story

- **Status:** Proposed (pending architect ratification)
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

The editor/LSP recovery epic (#209) then hit the boundary the thesis warns about.
Its three children are not uniformly grounded:

- **#168** (interactive parser API) — Python Lark *has* an `InteractiveParser`
  (`accepts()`/`feed_token`/`resume_parse`/`copy`/`exhaust_lexer`). Porting it is
  **inside** the oracle: drive both engines through the same operations, compare.
- **#164** (automatic richer strategies: token insertion / sync-point panic mode)
  — Python has **no** automatic equivalent (it delegates to the user via the
  interactive parser). No oracle.
- **#165** (inline `ERROR` nodes in the tree) — Python deliberately keeps errors
  *alongside* the tree, so an in-tree shape has **no** Python counterpart.

That asymmetry is a genuine fork (filed #211): what is the falsifiable acceptance
basis for behaviour Python doesn't have? Absent a written rule, an agent either
guesses (unsafe) or stalls. The decision is the architect's because "how far past
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

### Applied to the recovery epic (#209), resolving #211

- **#168 — accept.** Python has it ⇒ oracle-backed. Port the **oracle-backed
  subset** of the `InteractiveParser` surface (no new *parser behaviour* beyond
  Python), validated by a step-granular differential (`accepts()` traces + result
  trees) plus relative-oracle property tests (resume == parse, exhaust+eof == parse,
  `accepts()` honesty). Becomes `good-autonomous`; merges escalate-tier (new public
  API, §6).
  - **The surface rule.** No new parser *behaviour* beyond Python. Rust *may* expose
    convenience *spellings* that lower directly to a Python-backed operation, each
    with its own thin test — e.g. `feed(name, value)` is sugar over Python's
    `feed_token` (caller-directed insertion, in scope). It need not clone Python's
    spelling byte-for-byte, and may omit Python ops it doesn't need (`choices()`),
    but it must add no operation that does something Python's parser cannot.
    *Automatic* insertion (#164) is new behaviour, so it is **not** in scope.
- **#164 — defer** (beyond-oracle, no demand). Keep `prio:later`; revisit only on
  a concrete consumer + a validation story. Once #168 ships, callers can do their
  own insertion/resync through an oracle-backed surface, which is the Python model
  and largely subsumes #164.
- **#165 — defer** (beyond-oracle). If demand arrives, its validation story is the
  *projection invariant* (strip the in-tree error decorations ⇒ byte-identical to
  the oracle-backed "alongside" tree, over the whole recovery bank) + curated
  goldens for marker placement only. Until then, `prio:later`.

## Consequences

- **#211 is resolvable.** Each child has a written, falsifiable done-when (or a
  deferral reason); the `needs-decision` flag comes off and the children are
  re-labelled (architect action on merge): #168 → `good-autonomous`, #164/#165 →
  `prio:later` with the deferral recorded.
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
