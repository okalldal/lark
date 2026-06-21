# ADR-0024: CYK empty-rule rejection is keyed on source provenance, not name spelling

- **Status:** Proposed
- **Date:** 2026-06-21

## Context

Python Lark's CYK backend rejects a grammar whose CNF conversion would emit an
empty rule (`CYK doesn't support empty rules`). lark-rs must match that rejection
(invariant §2; [ADR-0017](0017-oracle-fidelity-is-for-intended-behavior.md): being
*more permissive* than the oracle is unfalsifiable). #101 found lark-rs *accepted* a
wholly-nullable transparent rule (`_a: B?`) that Python rejects: the original guard
keyed on `!info.inline`, so any transparent origin (leading `_`, or an `__anon_*`
helper) slipped through.

The obvious fix — drop the carve-out and reject *every* nullable `Nt::Orig` —
over-corrects. A `*`/`?` nested where a single symbol is mandatory (inside `~n`,
e.g. `start: A (B*)~2`) emits a **standalone nullable anonymous EBNF helper**
(`__anon_rep_*` / `__anon_group_*`). Python Lark's CYK **accepts** that grammar and
lark-rs matches it tree-for-tree today; the blunt rejection would start rejecting an
input Python parses — itself a §2 oracle regression. The four compliance banks miss
it because their only `~n` cases are on terminals (`"A"~2`), never a nullable group.

A differential audit (recorded on #101) pinned the exact discriminator: **Python
rejects ⟺ the nullable rule is user-written; it accepts iff every nullable origin is
a generated anonymous EBNF helper.** The remaining question was the *mechanism*: the
interner ([ADR-0003](0003-intern-symbols-to-ids-with-flags.md)) deliberately folds
`_name` and `__anon_*` into one `inline` flag and warns against name-prefix sniffing,
because a user grammar can author the exact name `__anon_star_0` (#144) — so a
`name.starts_with("__anon_")` gate in `cyk.rs` would reintroduce the bug under a
different spelling.

## Decision

CYK's empty-rule rejection is keyed on **source provenance**, not transparency and
not name spelling. The loader already mints every anonymous EBNF helper through one
typed choke point (`fresh_anon_rule(AnonKind)`); we record that `AnonKind` at mint
time, plumb it through lowering onto `SymbolInfo.anon_kind: Option<AnonKind>`, and
CYK rejects a nullable `Nt::Orig` **iff `anon_kind.is_none()`** — i.e. iff it is a
user-written rule. A user-authored rule named `__anon_star_0` has `anon_kind == None`
and is rejected like any other user rule; a generated `(B*)~2` helper has
`Some(..)` and is accepted, matching Python.

This refines the #101 line in [ADR-0017](0017-oracle-fidelity-is-for-intended-behavior.md)
("reject, restoring parity"): the rejection is restored for *user-written* nullable
rules only — generated helpers Python keeps are kept.

## Consequences

- Parser code consults structural metadata, never symbol spelling — consistent with
  ADR-0003 and the #144 release-only hazard. `anon_kind` is a new `SymbolInfo` field
  carried from the loader; `Grammar` gains an `anon_kinds: HashMap<String, AnonKind>`
  side table populated by `fresh_anon_rule`.
- We match Python on both poles: `a: B?` / `_a: B?` rejected, `(B*)~2` / `(B?)~2`
  accepted and tree-identical.
- Cost: a small amount of plumbing (loader → `Grammar` → `lower` → `SymbolInfo`) and
  one more semantic axis to keep distinct from `inline`. The two are genuinely
  different — `inline` is a tree-shaping decision; `anon_kind` is source provenance —
  so conflating them was the original bug.
- **Tripwire to revisit:** if a real-world grammar (a wild-bank find) shows a nullable
  *user* rule that Python's CYK actually accepts, the discriminator is wrong — revisit
  here, not by re-adding a name-prefix check.
- Enforcement (`src/parsers/cyk.rs` tests + `src/grammar/intern.rs` test):
  `cyk_transparent_nullable_rule_diverges_from_oracle` (`_a: B?` rejected),
  `cyk_rejects_user_authored_anon_looking_nullable_rule` (user `__anon_star_0: B?`
  rejected — proves provenance, not prefix),
  `cyk_accepts_nullable_helper_under_rep_count` (`(B*)~2` / `(B?)~2` accepted, tree
  parity vs LALR ≡ oracle), and
  `anon_kind_marks_generated_helpers_not_user_rules` (the plumbing). The compliance
  and wild banks remain the dominant gate.
