# Decision memo — framing the validation story for editor/LSP-grade recovery

**Status:** decision-support for the architect. Drafts a recommendation for the
`needs-decision` issue **#211** (epic **#209**). Does **not** resolve it — per
`PRINCIPLES.md` §1/§5 an agent surfaces and proposes; the architect picks. No
labels were changed and no children were re-tiered.

**Author:** spike session, 2026-06-21.
**Reading time:** ~10 min. The recommendation is in §1; the rest is the grounding.

---

## 1. TL;DR — recommendation

The issue frames four candidate validation stories (property tests / partial
oracle / curated goldens / defer) as if we must *pick one*. We should not. The
right answer is a **layered validation doctrine, applied per-child, ordered by
falsifiability** (the §2.7 meta-invariant). The interesting finding of this spike
is that **two of the three gated children can be re-grounded against the existing
oracle-backed recovery path** — they are *not* as oracle-orphaned as the epic
text assumes — and only a small irreducible residue ever needs curated goldens.

Concretely:

| Child | Primary validation basis | Residue (curated) | Verdict |
|---|---|---|---|
| **#168** interactive parser API | **Partial oracle** — operation-for-operation differential against Python's `InteractiveParser` (`accepts()`, `feed_token`, `resume`, `copy`, `exhaust_lexer`) | property tests for any op Python doesn't expose | **promote to `good-autonomous`** |
| **#165** inline `ERROR` nodes | **Relative oracle** — *projection invariant*: strip the error decorations and the tree must equal the oracle-backed "alongside" tree, byte-for-byte; spans must cover exactly the recorded error ranges | curated goldens for *placement only* | **promote to `good-autonomous`**, opt-in mode |
| **#164** richer strategies | **Property tests** — termination/progress, re-parse-clean, bounded error count, and a *superset relative oracle* (recovers every input deletion-only does, never worse) | curated goldens for statement-grammar sync points | keep `prio:later`; **defer the automatic strategies** behind #168 |

**Sequencing (by oracle-ability, which is the dominant NFR axis §4):**
**#168 first → #165 second → #164 last or won't-do.** Build the most-falsifiable,
highest-leverage surface first; let it subsume the least-falsifiable one.

**The one genuine fork for the architect** (everything else follows from
doctrine): *is the layered doctrine itself acceptable as the falsifiable basis,
and do you accept curated goldens as a legitimate Tier-C net for the named
residues?* If yes, #168 and #165 get falsifiable done-whens and leave the
`needs-decision` queue; #164 stays deferred. If you want **zero** non-oracle
validation anywhere, then #165's inline mode and #164 cannot be made autonomous
and should be closed as won't-do-without-demand (candidate 4).

---

## 2. The issue at stake

#211 is the **blocker** on epic #209 (*editor/LSP-grade error recovery*). The
architect approved the **direction** on 2026-06-21, but the epic cannot become
`good-autonomous` until one question is answered: **what falsifiable acceptance
basis must each child meet before an unattended `/next-task` may pick it?**

This is a `needs-decision` item because of the project's foundational thesis
(`PRINCIPLES.md` §0):

> The boundary of safe autonomy is the boundary of what we have made falsifiable.

Single-token-deletion recovery (#43/#94, shipped) was safely autonomous because
it is **byte-for-byte oracle-checkable**: it is exactly what Python Lark's
`on_error=lambda e: True` does, over the same LALR tables, so the oracle is
"does the recovered tree + deletion count match Python?" The next tier has no
such free oracle:

- **#164** (token insertion / sync-point panic mode) — *no Python equivalent.*
- **#165** (inline `ERROR` nodes) — *breaks oracle parity by design.*
- **#168** (interactive-parser API) — *only partly* oracle-able.

So the gate is not the implementation — it's the **validation story**. Until it's
framed, the three children stay `prio:later` and are not auto-picked (#209).

### What is *not* at stake (already resolved, for context)

The #95 design review spun out five sub-decisions. Two are **done**, which
narrows #211's scope:

- **#166** (contextual root-lexer fallback during recovery) — ✅ closed; it *had*
  an oracle (Python recovers over its contextual lexer) so it was groundable.
- **#167** ($END returns a marked/optional partial, not a fabricated derivation)
  — ✅ closed; `RecoveredTree.tree` is now `Option`, `None` at premature EOF
  (ADR-0019), oracle-pinned to Python's `recovered: false`.

So #211 is *only* about the three remaining oracle-orphaned children. Recovery on
Earley/CYK is already settled won't-do (no `on_error` upstream → unfalsifiable;
`PHASE_3_RECOVERY_PLAN.md` §1).

---

## 3. How this relates to the product goals and NFR priorities

### 3.1 Product goal

lark-rs's north star (`lark-rs/CLAUDE.md` "Goal") is a faithful Rust rewrite of
Lark that preserves its differentiators while gaining 10–100× speed and
multi-target distribution. Recovery is squarely *in scope* — #43's done-when
calls it "required for editor tooling, LSP use cases, and user-friendly
diagnostics," and the whole epic #209 is named for that audience. So the
direction is not in question; **only the validation basis is.**

The audience matters for the recommendation: editor/LSP integrations are the
intended consumers. That audience overwhelmingly wants (a) **a driveable parser**
they can feed/inspect to do their own recovery (#168) and (b) **errors located in
the tree** so they can highlight a subtree (#165). A *cleverer built-in automatic
panic mode* (#164) is the least of what an LSP wants once it can drive recovery
itself — which is the core of the sequencing argument in §6.

### 3.2 NFR priorities — this repo's NFR layer *is* `PRINCIPLES.md` §4

This repo deliberately does **not** keep a prose NFR document (an external
taxonomy like ISO/IEC 25010 is "at most a one-time coverage check, never working
vocabulary," §4). Instead each quality must resolve to **a gate or a named ADR
axis**. The §4 decision lens, applied to this decision:

| §4 dimension | Its gate | Bearing on #211 |
|---|---|---|
| **Correctness & Python parity** — *the dominant axis* | oracle + compliance/wild banks | This is the whole problem: the new behavior has no direct oracle. The recommendation's job is to **manufacture the most oracle-like gate available** for each child (partial oracle > relative oracle > property test > curated golden). |
| **Performance & complexity class** | work-counters + scaling gates | A new algorithm with no envelope must "propose the gate first." #164's panic mode and #168's `feed`/`resume` need a **termination/progress** counter so adversarial input can't loop — see §3.3. |
| **Maintainability & simplicity** | `/code-review`; consolidate-seams-before-features (ADR-0015) | #168 is a *large new public API surface*; favor exposing the existing `run_recovering` value+state-stack seam, not a parallel engine. |
| **Portability (PyO3/WASM/C/standalone)** | CI binding jobs; const-bakeability | #168's `InteractiveParser` must bake into every target. The Tree worklist rewrite (#151) is the precedent that a recursive API can be a WASM-stack hazard — name it. |
| **Security/robustness on adversarial input** | scaling gates catch algorithmic-complexity DoS; **otherwise largely ungated — judgment + flag the gap** | Recovery runs on *untrusted, broken* input by definition. "Recovery always terminates and makes progress" is the exact ungated-NFR row §4 flags. The property-test tier **converts that row into a gate** — a strict win that the architect's own §0 ("build the missing gate") asks for. |
| **API & grammar-author ergonomics** | **none — judgment-only** | #168's surface shape and #165's `ERROR`-node spelling are ergonomic calls; §4 requires they be **named in the ADR**, not gated. |

The takeaway: §4 doesn't just permit a layered, falsifiability-ordered doctrine —
it *prescribes* it. "A gated dimension is decided by its gate; a material
judgment-only dimension must be named in the ADR." The doctrine below is that
rule applied to recovery.

### 3.3 The robustness angle is a free win worth calling out

Single-token-deletion's termination is trivial (every step advances toward
`$END`; `PHASE_3_RECOVERY_PLAN.md` §4). Token **insertion** (#164) and a caller
driving **feed** (#168) both break that guarantee — an insertion that re-enables
the same error, or a handler that feeds forever, can loop. Today this is an
*ungated* NFR. The property-test tier makes "recovery consumes input or halts
within a bounded number of synthetic steps" a **committed deterministic gate**
(it pairs naturally with the `perf-counters` discipline, §2.5). Independent of
which children ship, **this gate should be built** — it is exactly the
gate-building §0 names as the architect's highest-leverage work.

---

## 4. The proposed doctrine: three tiers, falsifiability-ordered

The §2.7 meta-invariant ("falsifiable over aspirational") gives a total order on
validation strength. Apply the strongest tier each child's behavior admits, and
only fall to a weaker tier for the *residue* the stronger one can't reach.

- **Tier A — Oracle / partial oracle (strongest).** Differential against Python
  Lark. For #168, drive the *same operation sequence* on both `InteractiveParser`s
  and compare results, for every operation Python exposes. This is the §2.2
  invariant in its purest form and needs no new policy.

- **Tier B — Relative oracle & property tests (engine-agnostic, still
  falsifiable).** Two sub-kinds:
  - **Relative oracle** — re-ground the new behavior against the *existing
    oracle-backed path*. The key insight of this spike: #165's inline tree is a
    *decoration* of the oracle-backed "alongside" tree, so **projecting the
    decorations away must reproduce the oracle tree exactly.** That transports
    the Python oracle onto a feature Python doesn't have. #164 gets a weaker
    relative oracle: it must be a *superset* of deletion-only (recover every input
    deletion recovers; never produce a worse outcome).
  - **Property tests** — invariants that hold for *any* correct recovery: the
    recovered tree re-parses cleanly under the same grammar; the error count is
    bounded by the number of error positions; recovery terminates/makes progress
    (§3.3); recovery is idempotent on already-clean input.

- **Tier C — Curated golden trees (weakest; pinning, not grounding).**
  Hand-authored expected outputs, reviewed **once** and pinned, explicitly marked
  *not* oracle-derived (so the freshness gate never tries to regenerate them).
  Legitimate **only** for the irreducible residue Tiers A/B cannot reach — e.g.
  the *exact placement* of an `ERROR` node, or *which* sync point a panic mode
  picks. The §4 rule binds: each curated residue is a judgment-only dimension and
  must be **named in the child's ADR**.

**Doctrine rule:** a child is `good-autonomous` iff its done-when names (a) the
highest tier it reaches for its core behavior and (b) every residue pushed to
Tier C, each with a one-line justification. "Curated goldens for everything" is
*not* acceptable — that's the aspirational quadrant §2.7 forbids.

---

## 5. Per-child validation story (the deliverable #211 asks for)

### #168 — interactive parser API → **Tier A (partial oracle)**

Python's `on_error` receives a real `InteractiveParser` exposing `accepts()`,
`feed_token`, `resume_parse`, `copy`, `exhaust_lexer`, `pretty`. For **exactly
that surface**, a differential oracle is straightforward and strong: capture, for
a sequence of operations on a broken input, Python's `accepts()` sets after each
feed, the final tree after `resume`, and the copy/exhaust behavior; assert lark-rs
matches. lark-rs already has the substrate — `run_recovering` drives the LALR
value+state stack (`PHASE_3_RECOVERY_PLAN.md` §4); #168 *exposes* that seam rather
than building a parallel engine (ADR-0015 consolidate-seams).

- **Tier A:** operation-for-operation differential on Python's surface.
- **Tier B:** for any operation lark-rs adds beyond Python's surface, property
  tests — chiefly `feed`→`accepts` consistency (a token in `accepts()` must feed
  without error) and `resume`-equals-normal-parse (resuming a fully-fed valid
  stream yields the same tree as `parse()` on the equivalent input).
- **Tier C:** none expected.
- **Named ADR axes (§4):** API ergonomics (the Rust surface shape — builder vs.
  handle, ownership of the stack); portability (must bake into PyO3/WASM/C —
  watch the #151 recursive-Drop hazard).

**Done-when (proposed):** *an `InteractiveParser`-like API over the LALR
value+state stack, with a differential oracle (`tools/generate_oracles.py`)
pinning `accepts()`/`feed`/`resume`/`copy`/`exhaust` against Python's
`InteractiveParser` for the shared surface, plus property tests for any
lark-rs-only operation.* → `good-autonomous`.

### #165 — inline `ERROR` nodes → **Tier B (relative oracle) + Tier C residue**

The epic says #165 "breaks oracle parity by design." True for the *default*
oracle — but the inline tree is the alongside tree **plus** error decorations, so
the parity is recoverable by **projection**:

- **Tier B (relative oracle), the spike's main finding:** define a projection
  `strip_errors(inline_tree)` that removes `__error__` nodes / error-span `Meta`.
  Invariant: `strip_errors(inline_tree)` **==** the oracle-backed "alongside"
  tree, byte-for-byte, on the *entire existing recovery oracle bank*. That re-uses
  every committed Python oracle to gate the inline mode for free, and structurally
  guarantees inline mode is a pure superset (it can't *change* the recovered
  parse, only annotate it). Plus a span-coverage property: the union of `ERROR`
  spans equals the byte ranges of the recorded `RecoveredTree.errors`.
- **Tier C residue:** the *placement* of an `ERROR` node (nearest enclosing `Tree`
  vs. spliced at a reduction boundary — the open (a)/(b) choice in #165) is not
  determined by the projection. A handful of curated golden trees on
  statement-structured grammars, reviewed once, pin placement.
- **Named ADR axes (§4):** ergonomics (the `ERROR`-node spelling and the opt-in
  flag), and the (a)-vs-(b) placement decision itself.

**Done-when (proposed):** *opt-in inline mode where `strip_errors(tree)` equals
the default alongside-tree on the whole recovery oracle bank, error spans cover
exactly the recorded error ranges, and curated goldens pin node placement; the
default stays oracle-checkable "alongside."* → `good-autonomous`, opt-in only.

> Note: the (a)/(b) placement choice is itself a small `needs-decision`-shaped
> fork. It's narrow enough to fold into #165's ADR once the doctrine is accepted,
> but flag it if you want to pre-decide.

### #164 — richer strategies → **Tier B property tests; recommend defer**

#164 has the weakest oracle story and the smallest marginal audience.

- **Tier B:** termination/progress gate (§3.3 — the robustness win); recovered
  tree re-parses cleanly; error count bounded; **superset relative oracle** —
  every input deletion-only recovers, the richer strategy recovers to an
  equal-or-better outcome (a tree where deletion gave `None`, or the same tree
  where both succeed). This is falsifiable but does **not** pin *which* richer
  tree — only that it's no worse.
- **Tier C residue:** "the right tree" for token insertion / which sync point a
  panic mode resyncs to is genuinely a judgment call with no oracle. Curated
  goldens on statement grammars, reviewed once. This residue is **larger** than
  #165's — most of #164's *value* (smarter trees) lives in the un-grounded part.

That asymmetry is the recommendation: **defer #164's automatic strategies.** Once
#168 ships, a caller can drive token insertion and custom resync **themselves**,
through an oracle-backed interactive surface — capturing most of #164's value
*with* a falsifiable basis and *without* committing the project to maintain
un-oracle-able heuristics on the hot path. #164 becomes "won't-do without
demand" (candidate 4) unless a concrete consumer asks for a *built-in* strategy.

**Done-when (if pursued anyway):** *property tests (terminates/progresses,
re-parses clean, bounded errors, superset-of-deletion) + curated goldens for sync
behavior on a statement grammar.* Otherwise keep `prio:later` / close as
won't-do-without-demand.

---

## 6. Sequencing & why

Order by oracle-ability (= the dominant §4 axis), which also happens to be
audience-leverage order:

1. **#168 first.** Most oracle-able (Tier A), highest leverage — it's the
   substrate LSPs actually want, and it **subsumes** #164: once callers can feed
   tokens and inspect `accepts()`, built-in insertion is mostly redundant.
2. **#165 second.** Tier-B relative oracle grounds it cheaply; it directly serves
   the "highlight the broken subtree" LSP need; opt-in, so it never threatens the
   default's oracle parity.
3. **#164 last, or won't-do.** Least falsifiable, smallest marginal value after
   #168. Defer unless a consumer demands a built-in strategy.

This ordering is itself an application of §0: do the work whose correctness we can
*check* first, and let it shrink the work whose correctness we can only *assert*.

---

## 7. What the architect actually needs to decide

The recommendation reduces #211 to a small number of explicit calls:

1. **Accept the layered doctrine** (§4) as the falsifiable basis — i.e. accept
   that a partial/relative oracle plus property tests, with **named** curated-Tier-C
   residues, makes a child `good-autonomous`. (If you reject Tier C entirely,
   #165's inline mode and #164 become won't-do — candidate 4.)
2. **Promote #168 and #165** to `good-autonomous` with the done-whens in §5;
   record the doctrine as an ADR (this is `escalate`/governance-tier per §6/§9, so
   it rides its own PR and you merge it).
3. **Defer #164** (keep `prio:later`, or close as won't-do-without-demand).
4. **Adopt the termination/progress gate** (§3.3) regardless — it closes a
   standing ungated-NFR row.
5. *(Optional)* pre-decide #165's (a)-vs-(b) node placement, or leave it to
   #165's ADR.

Then remove `needs-decision` from #211, write the chosen done-whens onto
#164/#165/#168, and re-label per the calls above (#209's "done-when").

> Per `PRINCIPLES.md` §5, none of this was actioned by this spike — labels,
> re-tiering, and the ADR are the architect's to make. This memo is the proposal.
