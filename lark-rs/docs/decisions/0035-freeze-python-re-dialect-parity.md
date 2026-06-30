# ADR-0035: Python `re` dialect parity is frozen pending a user-needs signal

- **Status:** Accepted (2026-06-30; architect ratified)
- **Date:** 2026-06-30

> Forward-looking *policy* ADR (architect product-direction). There is no oracle
> for "which terminal-regex dialect divergences are worth chasing, and behind what
> API" — that is the architect's to set, and it cannot be set responsibly without
> user signal we do not yet have (ADR-0025: no users). Relates to ADR-0017 (oracle
> fidelity is for *intended* behaviour) and ADR-0026 (behaviour scoped to the
> oracle); supersedes neither.

## Context

lark-rs lexes terminals with the Rust `regex` / `regex-automata` engine; Python
Lark uses CPython `re`. The two are different regex *dialects*, and a recurring
stream of `needs-decision` issues (#275, #288, #332, #363, #365, #415, #461)
records points where they assign **different meaning to the same terminal syntax**.
Each has been escalated as an individual "support (match Python) vs. categorized
refusal" fork.

Three facts make resolving them *now* premature:

1. **No consumer signal.** lark-rs has no users yet (ADR-0025). Every one of these
   divergences is a corner of Python `re`; none has a reported grammar that needs
   it. Deciding them is guessing at a distribution we cannot yet observe.
2. **The cluster is small and already inert.** It is ~5% of production code, and
   the divergences are already encoded as **XFAILs** — known, pinned,
   non-regressing. The cluster sits in a parked state *by construction*; the only
   open question is whether to *spend* effort burning it down.
3. **Piecemeal fixes pre-commit an architecture.** A larger fork sits behind the
   small ones: do we (a) maximise Python compatibility, (b) adopt regex-crate
   semantics, or (c) offer both as selectable modes? Each ad-hoc "normalise
   construct X toward Python" fix is an implicit vote for (a) and adds
   path-dependence that makes (b)/(c) harder to reach. Resolving the dialect issues
   one at a time decides the architecture *by accident*.

## Decision

**Freeze work whose purpose is to bring terminal-regex *match semantics* closer to
Python `re`** (i.e. to flip a dialect XFAIL toward Python-parity), and **defer the
dialect-mode architecture decision**, until a user-needs signal arrives.

The already-shipped posture is **unchanged**: the existing `\<` / `\>`
normalization and every committed dialect XFAIL stay exactly as they are. This ADR
stops *new* parity work and defers the architecture question; it reverses nothing.

Frozen issues carry the `frozen` label (`LABELS.md`) and drop out of the
`/next-task`, `/xfail-burndown`, and bug-hackathon queues. New dialect divergences
found by the differential fuzzer are **catalogued as XFAIL and parked**, not
triaged into fix-work.

## Scope — what this does *not* freeze

- **Leak / categorization hygiene.** Turning a raw, *uncategorized* engine error
  (e.g. #275's `\b` leaking a `regex-automata` internal string) or a
  *mis-categorized* refusal (e.g. #275's `\Z` reported as a lookaround scope) into
  a clean, categorized `GrammarError` — **without changing which inputs are
  accepted or rejected** — is policy-neutral and stays `good-autonomous`. The
  freeze is on matching Python's *result*, not on error quality.
- **Non-dialect bugs in the same files.** #349 (the DFA `2^N` determinization
  pathology — a resource bug, not a dialect question) and #416 (the PyO3 `Token`
  eq/hash bug — a binding bug) are explicitly **not** frozen and keep their
  priority.
- **Structural cleanup.** The `lookaround/lower.rs` / `terminal.rs` de-duplication
  (#478) may proceed on its standalone merit, **provided it introduces no
  behaviour change and no dialect-mode machinery**.
- **Lookaround *capability*.** The existing bounded-lookaround lowering is
  unaffected; this ADR concerns *dialect semantics over the shared feature set*,
  not which constructs lark-rs can express.

## Consequences

- The `needs-decision` dialect cluster leaves the architect's *active* inbox
  without being resolved — it is **deferred with a written trigger**, not closed.
- No architecture is pre-committed: maximise-compat, regex-crate semantics, and a
  two-mode split all remain open, and the eventual decision is made on *user
  evidence* rather than accreted fixes.
- The XFAIL banks may grow as the fuzzer keeps finding divergences; that is
  expected and is the price of parking. Growth is visible (the XFAIL ledgers) and
  reversible.
- **Tripwire — un-park.** The freeze ends when **either** (1) a real user grammar
  is broken by a dialect divergence (a concrete consumer demand — the ADR-0026
  bar), **or** (2) the architect decides the dialect-mode architecture independent
  of a specific report. At that point, supersede this ADR with the chosen policy
  (and, if modes are adopted, the mode API is `escalate`-tier per ADR-0025 / §6)
  and burn the cluster down under it. Do not let the freeze outlive its trigger,
  and do not resolve a frozen issue piecemeal while the freeze stands.
- Enforced by: the `frozen` label (`LABELS.md`) gating the work-selection commands;
  this ADR cited from the *"Terminal regexes are Python-`re` dialect"* note in
  `lark-rs/CLAUDE.md`; and the tracking issue that lists the frozen set and the
  un-park trigger.
