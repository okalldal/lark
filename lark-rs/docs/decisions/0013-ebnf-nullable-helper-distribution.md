# ADR-0013: EBNF nullable helpers — distribute non-final, share only the recurse core

- **Status:** Accepted
- **Date:** backfilled 2026-06-13 from PRs #98, #99, #100, #138
- **Amended:** 2026-06-23 (#272, RC7) — added the post-lowering reduce/reduce
  collision audit (see "Amendment" below). The sharing is unchanged; the audit is
  a build-time gate layered on top of it.

## Context

Expanding EBNF (`a*`, `(a b)?`, `x?`) into anonymous helper rules has a subtle
correctness boundary that interacts with *both* the LALR automaton and the
contextual lexer. Two opposing pressures: we want to **share** structurally
identical helpers (smaller tables, Python's `rules_cache` parity), but sharing
the wrong ones breaks parsing. The rule that resolves this lived only in PR
discussions.

## Decision

Mirror Python Lark's `SimplifyRule_Visitor`/`rules_cache` split:

- **A non-final nullable helper is distributed into the parent's alternatives,
  not kept as a shared node.** Why (#100): *"a non-final nullable helper … the
  closure never expands `Y`. The LALR automaton therefore mispredicted."* Under
  `maybe_placeholders` the distribution also threads `nones_before` so non-final
  optionals don't hide LALR branches (#138, fixing #106).
- **Only the `*`/`+` recurse core is shared; `Opt`/`Maybe`/`GroupOptional` are
  NOT shared.** The gotcha (#98): *"Sharing a nullable helper across two parents
  unions their follow-sets, and the contextual lexer derives each state's scanner
  from those follows, so an over-merge silently widens a state's terminal set and
  breaks state-narrowing (it made csv.lark's `header` start trying `row`'s
  terminals)."*

## Consequences

- Over-sharing is a *silent* correctness bug, not a crash: it widens a contextual
  lexer state's terminal set, defeating the state-narrowing that is Lark's whole
  point. Any future helper-dedup change must respect the nullable boundary.
- The constraint couples three subsystems — EBNF expansion, the LALR closure, and
  the contextual lexer's per-state scanner derivation — so it can't be reasoned
  about in the loader alone. Pinned by `tests/test_*` follow-union parity tests
  added in #99.

## Amendment (2026-06-23, #272 / RC7 — architect chose Option A)

**Problem.** The recurse-core sharing above is *coarser* than Python Lark's. Python's
`EBNF_to_BNF._add_recurse_rule` keys its grammar-wide `rules_cache` on the inner
`expr` **Tree**, so `r0*` (inner `value(r0)`) and `(r0)*` (inner
`expansions(expansion(value(r0)))`) get **distinct** star helpers that
reduce/reduce-collide — Python rejects `start: r0* | (r0)*` at build. lark-rs's
`recurse_cache` keys on the *compiled arms*, which collapse the single-symbol group
wrapper (`compile_group`'s shortcut), so both occurrences **share one** helper and
the collision never forms — lark-rs accepted the grammar, masking the ambiguity
(RC7, bug bounty). The conflict detector in `parsers/lalr.rs` is correct; it simply
never saw two rules.

**Why not just un-share.** Measured: gating off the `recurse_cache` sharing regresses
the LALR compliance bank **512 → 482** (−30 grammars). The sharing is load-bearing
exactly as this ADR's body warns (contextual-scanner width + dynamic-lexer resolve
order; #91/#32/#90/#210). Un-sharing is off the table.

**Decision (Option A).** Keep the sharing for the real parse table. Add a
**post-lowering reduce/reduce collision audit**: when the loader detects that a real
`recurse_cache` hit fused two occurrences with *distinct inner source-AST*
(`recurse_overshare_seen`), it builds a Python-faithful **audit shadow** — the same
grammar re-lowered with recurse helpers keyed on the inner source-AST
(`Expr::python_recurse_key`, `GrammarCompiler::python_keyed_recurse`), so the helpers
split exactly as Python mints them. The LALR build (`parsers/mod.rs::build_lalr`) runs
the **same** conflict detector over the shadow's lowering and surfaces any
`GrammarError::Conflict` it reports. The shadow is build-gating only; it never parses,
and the real `recurse_cache` is untouched.

**Why it matches the oracle exactly (not over-reports).** The shadow is structurally a
*superset* of the real grammar's recurse rules (helpers split, never merged), and the
audit re-uses the real true-LALR(1) detector — so it can only ever expose the masked
collision, never invent a spurious one. Crucially, a purely structural "distinct AST ⇒
reject" rule would **over-reject** (`start: A r0* | B (r0)*` splits into two helpers
but they sit behind distinct terminals `A`/`B`, never reach a common state, and Python
*accepts* it) — which is why the audit runs the real detector rather than a structural
shortcut. Verified against Python Lark 1.3.1 over the full differential family in
`tests/test_bounty_findings.rs` (`rc7_reduce_reduce_differential_matches_oracle`):
rejects `r0*|(r0)*`, `r0+|(r0)+`, arm-order, nested, tail-guarded, two-rule `x:a+/y:a+`,
cross-rule `p:r0* q:(r0)*`, `foo:WORD+/bar:WORD+`, `(",",X)*` twice; accepts
`r0*|(s0)*`, `A r0*|B (r0)*`, `a+ b|a+`, `a* b|a+`, `r0*|r0*`, `single (",",X)*`. The
**LALR compliance bank stays 512/512** — the audit adds rejections only where Python
rejects.

**Consequences.** The reduce/reduce rejection contract now spans the loader (which
mints the audit shadow when over-share is detected) and `build_lalr` (which runs the
detector over it). A future change to the recurse-helper sharing key must keep
`Expr::python_recurse_key` aligned with Python's `_add_recurse_rule` keying, or the
audit will drift from the oracle. The audit costs one extra lowering pass **only** for
grammars that actually over-share a recurse helper (`recurse_overshare_seen`); grammars
that don't pay nothing.

**Public-API note (no break).** The audit adds a field `Grammar::lalr_audit:
Option<Box<Grammar>>` to the surface `Grammar` struct. It is internal build machinery —
set by the loader, read only by the LALR build, the import resolver, and standalone
generation, all inside the crate — so it is declared **`pub(crate)`**, not `pub`. That
keeps the field out of the public API: it is **not** a public struct-shape change (a
downstream struct literal cannot name a `pub(crate)` field, and `Grammar` already
carries private fields, so it cannot be built externally by struct literal regardless).
A grep of every read/write site confirmed nothing outside the crate (bindings,
examples, tests) touches the field, and `cargo build --all-targets` is clean. The
derived `Clone` deep-copies the box like any other field, but `Grammar` is not cloned
on any build path, so the shadow is never duplicated in practice. (An earlier revision
of this PR declared the field `pub`; the review-hardening commit narrowed it to
`pub(crate)`, eliminating the API break.)

## Amendment 2 (2026-06-23, #272 follow-up — audit propagates through `%import`)

**Problem.** The Amendment-A audit shadow was attached to the `Grammar` returned by
`load_grammar_with_sources`, but `%import` resolution (`grammar/loader/imports.rs`)
compiled imported files through the normal loader and copied their rule closure out
via `copy_requested` **without carrying or rebuilding `lalr_audit`** — and the shadow
compiler's own import resolution still ran the normal (non-Python-keyed) loader path.
So an RC7 over-share living **inside an imported file** (`%import .bad (bad)` with
`bad: r0* | (r0)*`) — or reached through nested imports — was accepted by lark-rs where
Python rejects it: the same masked collision, one `%import` away. Confirmed against
Python Lark 1.3.1 (`Lark.open`, parser="lalr"): the imported / nested forms reject
exactly as the direct form does.

**Decision.** Make import resolution **audit-aware**, in `imports.rs::copy_imported`
(the new single choke point both import call sites route through):

- **Real (sharing) pass.** If an imported grammar carries `lalr_audit` (it built its
  own shadow because it over-shares internally), propagate the signal by setting the
  parent's `recurse_overshare_seen`, so the parent builds an audit shadow at all — but
  still copy the imported grammar's *shared* rules into the real parse table (ADR-0013
  sharing untouched).
- **Audit (shadow) pass.** When the parent's Python-keyed shadow resolves the same
  import and the imported grammar carries `lalr_audit`, copy the closure from the
  imported grammar's **shadow** (its split, Python-keyed recurse helpers) instead of
  its shared real rules — so the masked collision reaches the parent's audit. An
  imported grammar with no internal over-share has `lalr_audit == None`; its real rules
  are already Python-faithful for that file. An *import-straddling* over-share (the
  shared inner rule imported, the colliding `r0*` / `(r0)*` helpers minted locally) is
  re-lowered Python-keyed by the parent shadow itself and needs no special handling.

The cheap pre-scan that decides whether to clone the AST for a possible shadow
(`items_need_audit_clone`, was `items_contain_repeat`) now also returns `true` for any
grammar containing an `%import`, since an imported over-share can flip
`recurse_overshare_seen` even when the parent body has no repeat operator of its own.
Verified against Python Lark 1.3.1 across the direct + import families (whole-imported,
`+` variant, straddling, parent-own-overshare-plus-import, nested) for both reject and
genuine-accept cells in
`rc7_reduce_reduce_differential_matches_oracle_via_import`; the **LALR compliance bank
stays 512/512**.
