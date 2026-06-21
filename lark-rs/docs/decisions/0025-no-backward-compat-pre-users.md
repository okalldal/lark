# ADR-0025: Pre-users — breaking the public API is free

- **Status:** Proposed (pending architect ratification)
- **Date:** 2026-06-21

> Forward-looking *policy* ADR (architect product-direction, not a backfilled
> historical record). It adds a `PRINCIPLES.md` §3 default and is genuinely the
> architect's to set — there is no oracle for "do we owe anyone compatibility?"

## Context

lark-rs has **no users yet**. Nothing depends on its public surface (`Lark`,
`LarkOptions`, `ParserAlgorithm`, the PyO3 / WASM / C bindings, the standalone
emitter API). That makes backward compatibility a cost with, currently, zero
benefit: a deprecation shim, a kept-around old signature, or a "rename would
break callers" hesitation buys safety for callers who do not exist.

Absent a written rule, an agent mid-refactor defaults to caution — preserving an
old shape, adding a compat path, or tiering a clean rename as risky — because
"don't break the public API" is the universal background assumption everywhere
*else*. That caution is pure waste at this phase, and it is invisible: it shows up
as roads not taken, not as a diff to review.

This is a **phase-conditional** fact, which is why it is an ADR (dated,
supersedable) rather than a §2 invariant (permanent, never-violate). The opposite
shape: §2 is a constraint you may never relax; this is a *freedom* that ends the
moment lark-rs has a real dependent.

## Decision

Until lark-rs has users, **breaking the public API is free.** Do not preserve
deprecated shapes, add compatibility shims, or keep an old signature alongside a
new one for compatibility's sake. Prefer the clean shape; delete the old one.

Recorded as a `PRINCIPLES.md` §3 default so it is in the always-loaded layer that
shapes every session (an ADR alone is cited for depth, not in-context).

## Scope — what this does *not* change

- **It does not touch the §6 merge tiers.** New or changed public API stays
  `escalate`-tier. API changes are escalate because they are *design / product
  direction* with no oracle (§4 lens: "API & grammar-author ergonomics —
  ungated; judgment-only"), **not** because of backward compatibility. Removing
  the compat concern does not make API design self-groundable, so it still goes
  to the architect.
- **It is not license to churn.** "Breaking is free" removes a *constraint* on
  good changes; it is not a *reason* to reshape API surface absent a design win.
  The §3 small-blast-radius default and the DoD still apply.

## Consequences

- Refactors take the clean path: rename, remove, and reshape without compat
  scaffolding or deprecation cycles.
- No deprecation machinery accretes before there is anyone to deprecate *for* —
  the surface stays as small as the current design justifies.
- **Tripwire — this ADR's expiry.** The first real dependent (a published
  release with semver expectations, an external consumer, a downstream pin)
  flips the calculus. At that point supersede this ADR with one that introduces a
  stability policy, and remove or rewrite the §3 default. Do not let the freedom
  outlive the precondition.
