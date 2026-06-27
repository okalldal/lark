//! Phase 4 — rule-body compilation: EBNF expansion, leading-nullable
//! distribution, and anonymous-helper sharing (AST `Expr`s → flat BNF [`Rule`]s).

use super::ast::*;
use super::audit::RecurseDecision;
use super::compiler::{AnonKind, GrammarCompiler};
use crate::error::GrammarError;
use crate::grammar::rule::{Rule, RuleOptions};
use crate::grammar::symbol::{NonTerminal, Symbol, Terminal};
use crate::grammar::terminal::{Pattern, PatternRe};

/// The flavour of anonymous EBNF helper a structural cache key describes. Two
/// helpers share a generated rule only when they agree on *both* their kind and
/// their compiled alternatives, so a `(",", X)` group never collapses into a
/// `(",", X)?` optional even though their alternatives coincide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum HelperKind {
    /// `(...)` — a plain spliced group.
    Group,
    /// `(...)?` / a placeholder-less `[...]` — a group plus an empty alternative.
    GroupOptional,
    /// `[...]` under `maybe_placeholders` — empty case emits `None` placeholders.
    Maybe,
    /// `x?` — the single-symbol optional wrapper (`P: x | ε`).
    Opt,
    /// `x*` — the nullable wrapper around the shared `+`-recurse helper.
    Star,
}

/// One alternative of a compiled expansion: its symbol sequence plus the
/// per-gap `None`-placeholder counts a distributed absent `[...]` left behind
/// (`gaps[i]` Nones go before symbol `i`; `gaps[len]` trail). The gap vector is
/// always `syms.len() + 1` long during compilation; it is stored on the rule
/// only when some entry is nonzero.
pub(super) type CompiledAlt = (Vec<Symbol>, Vec<usize>);

/// One compiled position of an expansion (see `compile_slot`): either a fixed
/// symbol sequence, or a distributable leading nullable contributing several
/// present-form alternatives that fan out across the parent's alternatives.
enum Slot {
    /// Contributes this exact symbol sequence at its position (usually one
    /// symbol). Covers every non-distributed position, including a *trailing*
    /// nullable's shared `__anon_*` helper.
    Fixed(Vec<Symbol>),
    /// A leading nullable distributed into the parent: these are the non-empty
    /// ("present") alternatives; the absent alternative is added during the
    /// cartesian product in `compile_expansion`, contributing `absent_nones`
    /// positional `_EMPTY` markers (Python Lark's `[_EMPTY] * FindRuleSize`).
    /// These are computed for *every* `[...]` regardless of `maybe_placeholders`
    /// to carry the absent arm's collision identity; they become `None` tree
    /// children only when `maybe_placeholders` is on — with it off they're
    /// stripped at rule-output storage (`stored_output_gaps`).
    Nullable {
        present: Vec<CompiledAlt>,
        absent_nones: usize,
    },
    /// A plain `(a|b)` group distributed into the parent: one alternative per
    /// arm, with **no** absent/ε arm (the group is not nullable). Python Lark
    /// never materializes a helper rule for an inline group —
    /// `SimplifyRule_Visitor.expansion` cartesian-products it into the parent
    /// at *every* position — and the helper form is not behaviour-preserving
    /// under LALR: a helper arm that duplicates another rule's RHS (e.g.
    /// `(atom_expr | list)` next to `?atom: ... | list`) makes two unit rules
    /// over one symbol, which collide as an unresolvable reduce/reduce where
    /// Python sees only a silently-resolved shift/reduce (wild bank: vyper).
    Choices(Vec<CompiledAlt>),
}

/// Structural identity of an anonymous EBNF helper: its kind, the enclosing
/// `keep_all_tokens` context, and the ordered, compiled `(symbols, gaps, alias)`
/// of each alternative. Identical keys reuse one generated rule — Python Lark's
/// `rules_cache`. Caching the *compiled* symbols (not the AST) means the sharing
/// composes bottom-up: a repeated `(",", X)*` shares its inner group, which lets
/// its `+`-recurse helper and `*` wrapper share in turn, collapsing what would
/// otherwise be duplicate nullable helpers that LALR cannot disambiguate.
pub(super) type HelperKey = (HelperKind, bool, Vec<(CompiledAlt, Option<String>)>);

impl GrammarCompiler {
    pub(super) fn compile_rule(&mut self, raw: RawRule) -> Result<(), GrammarError> {
        let keep_all = raw.modifiers.contains('!') || self.global_keep_all;
        let expand1 = raw.modifiers.contains('?');
        // Build-time placement validation Python Lark performs in
        // `_make_rule_tuple` and its compile loop (`load_grammar.py`): an inlined
        // (`_`-prefixed) rule may not use the `?rule` (expand1) modifier, nor carry
        // an alias on any alternative — both name a subtree that the `_`-prefix is
        // marked to splice away. A `!` modifier and a `?`/alias on a *normal* rule
        // stay legal — only the `_`-prefix + `?`/alias combination is rejected
        // (RC4a/RC4b).
        Self::validate_inlined_rule_placement(&raw.name, expand1, &raw.expansions)?;
        let origin = NonTerminal::new(&raw.name);
        // Make keep_all visible to placeholder counting while this rule's body
        // (and the anonymous rules it expands into) is compiled.
        self.current_keep_all = keep_all;

        // Each source alternative may distribute into several BNF alternatives
        // (a leading nullable fanned out), so `order` runs over the flattened
        // result rather than the raw alternatives — after the cross-alternative
        // dedup + collision check (Python numbers post-dedup too).
        let compiled = self.compile_alternatives(raw.expansions, &origin.name, true)?;
        for (order, ((expansion_syms, gaps), alias)) in compiled.into_iter().enumerate() {
            let options = RuleOptions {
                expand1,
                keep_all_tokens: keep_all,
                priority: raw.priority,
                nones_before: self.stored_output_gaps(gaps),
                placeholder_count: 0,
            };
            self.rules.push(Rule::new(
                origin.clone(),
                expansion_syms,
                alias,
                options,
                order,
            ));
        }
        Ok(())
    }

    /// Compile a list of source alternatives (`AliasedExpansion`s) into the deduped,
    /// collision-checked set of BNF `(CompiledAlt, alias)` pairs — the shared prefix
    /// of every rule-body emitter (`compile_rule` / `compile_group` / `compile_maybe`
    /// / template instantiation). Each source alternative may distribute into several
    /// BNF alternatives (a leading nullable fanned out), so `order` runs over the
    /// flattened result; the per-alternative top-level `alias` is carried through,
    /// while [`dedup_and_check_alts`](GrammarCompiler::dedup_and_check_alts) collapses
    /// byte-identical arms and raises Python's "Rules defined twice" on a genuine
    /// collision (Python numbers post-dedup too).
    ///
    /// `reject_nested` rejects a *nested* alias (inside a `(...)`/`[...]`) up front, as
    /// Python does for a rule body / template (RC4c). The group/`[...]` helper callers
    /// pass `false`: their own enclosing rule already ran the rejection, and a group's
    /// inner alias is the legitimate helper-naming case (`distributable_alternatives`
    /// handles it), so re-rejecting here would be wrong.
    pub(super) fn compile_alternatives(
        &mut self,
        expansions: Vec<AliasedExpansion>,
        parent: &str,
        reject_nested: bool,
    ) -> Result<Vec<(CompiledAlt, Option<String>)>, GrammarError> {
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(expansions.len());
        for alt in expansions.into_iter() {
            let alias = alt.alias.clone();
            if reject_nested {
                // A nested alias (inside a `(...)`/`[...]`) is not a tree label —
                // Python reads it as a rule reference and rejects (RC4c). The
                // alternative's top-level `alias` is the legitimate one and is kept.
                Self::reject_nested_aliases(&alt.expansion)?;
            }
            for alt_c in self.compile_expansion(alt.expansion, parent, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        Self::dedup_and_check_alts(parent, compiled)
    }

    /// Reject the two inlined-rule placements Python Lark rejects at build
    /// (`load_grammar.py`): an alias on, or the `?rule` (expand1) modifier on, a
    /// rule whose name starts with `_` (RC4a/RC4b). The same check guards a
    /// `_`-prefixed *template* name (e.g. `?_x{a}` / `_x{a}: a -> al`), which
    /// Python rejects identically. Returns `Ok(())` for every non-`_` rule —
    /// aliases and `?`/`!` on a normal rule remain legal.
    pub(super) fn validate_inlined_rule_placement(
        name: &str,
        expand1: bool,
        expansions: &[AliasedExpansion],
    ) -> Result<(), GrammarError> {
        if !name.starts_with('_') {
            return Ok(());
        }
        if expand1 {
            // Python: `_make_rule_tuple` — "Inlined rules (_rule) cannot use the
            // ?rule modifier." (`load_grammar.py`).
            return Err(GrammarError::Other {
                msg: "Inlined rules (_rule) cannot use the ?rule modifier.".to_string(),
            });
        }
        if let Some(alias) = expansions.iter().find_map(|e| e.alias.as_deref()) {
            // Python: the compile loop — "Rule <name> is marked for expansion (it
            // starts with an underscore) and isn't allowed to have aliases
            // (alias=<alias>)" (`load_grammar.py`).
            return Err(GrammarError::Other {
                msg: format!(
                    "Rule {name} is marked for expansion (it starts with an underscore) \
                     and isn't allowed to have aliases (alias={alias})"
                ),
            });
        }
        Ok(())
    }

    /// Reject a *nested* alias — one inside a `(...)` / `[...]` group — exactly as
    /// Python Lark does (RC4c). In Lark's grammar, `-> NAME` is legal only at the
    /// top level of an alternative; inside a group the `-> NAME` makes `NAME` a
    /// rule reference, which Python then rejects: "Rule 'NAME' used but not
    /// defined" when `NAME` is undefined, or an `AssertionError` ("Double alias not
    /// allowed") when it is. Either way the grammar is rejected, so lark-rs rejects
    /// a nested alias unconditionally rather than being more permissive than the
    /// oracle (ADR-0017 corollary). The reported name matches Python's common case
    /// (`(A -> foo)`, `(A -> foo)?`/`+`, `(A -> foo | B -> bar)`, `[A -> foo]`).
    /// Recurses through every `Group`/`Maybe`/`Repeat` in a rule body; the
    /// *rule-top-level* alias on each `AliasedExpansion` is left untouched (it is
    /// the legitimate alias).
    pub(super) fn reject_nested_aliases(exprs: &[Expr]) -> Result<(), GrammarError> {
        for expr in exprs {
            Self::reject_expr_nested_aliases(expr)?;
        }
        Ok(())
    }

    fn reject_expr_nested_aliases(expr: &Expr) -> Result<(), GrammarError> {
        match expr {
            Expr::Value(_) => Ok(()),
            Expr::Repeat { inner, .. } => Self::reject_expr_nested_aliases(inner),
            Expr::Group(alts) | Expr::Maybe(alts) => {
                for alt in alts {
                    if let Some(alias) = &alt.alias {
                        return Err(GrammarError::UndefinedRule {
                            name: alias.clone(),
                        });
                    }
                    Self::reject_nested_aliases(&alt.expansion)?;
                }
                Ok(())
            }
        }
    }

    /// Compile a list of `Expr` nodes into one or more alternative symbol
    /// sequences, creating auxiliary rules as needed for EBNF operators.
    ///
    /// A single source expansion can lower to **several** BNF alternatives:
    /// a *leading nullable* EBNF helper (`X?`, `X*`, or `[X]`) that is not the
    /// last symbol of the expansion is **distributed** into the parent's
    /// alternatives — `a: X? Y` becomes `a: X Y | Y` — exactly as Python Lark's
    /// `SimplifyRule_Visitor` does. This is required for correctness: a named
    /// nullable helper before further symbols hides those symbols from the
    /// textbook LR(0) closure (the dot never advances past the helper until it
    /// ε-reduces), so the LALR automaton mispredicts and a shift/reduce conflict
    /// against the hidden path silently drops it (#97). Under
    /// `maybe_placeholders`, a distributed `[X]`'s absent alternative records
    /// its `None` placeholders positionally on the rule
    /// (`RuleOptions::nones_before`, Python's `_EMPTY` markers →
    /// `empty_indices`; #106). A *trailing* nullable causes no such hiding, so
    /// it keeps its shared helper (the lower-churn variant of the fix — Python
    /// distributes those too, but the helper form is conflict-free and
    /// byte-identical in the tree).
    ///
    /// `tail_ctx` is whether this expansion's *own* last position is genuinely
    /// final in the rule it will land in. It is `false` when compiling the
    /// present forms of a nullable being distributed (`distributable_alternatives`):
    /// those symbols are spliced inline into the parent's alternatives mid-rule,
    /// so a "trailing" nullable inside them is not actually trailing — left as a
    /// helper it would re-create the LR(0) dot-hiding this distribution exists to
    /// remove (e.g. `python.lark`'s `["," SLASH ("," paramvalue)*]`, whose inner
    /// `*` lands before the `["," [starparams|kwparams]]` branch).
    pub(super) fn compile_expansion(
        &mut self,
        exprs: Vec<Expr>,
        parent: &str,
        tail_ctx: bool,
    ) -> Result<Vec<CompiledAlt>, GrammarError> {
        let n = exprs.len();
        // Cartesian product of each position's choices, building present-form
        // alternatives before the empty one (Python's distribution order). Each
        // accumulated alternative carries its gap vector (`gaps.len() == syms.len()
        // + 1`), threading distributed-absent `None` placeholders positionally.
        let mut acc: Vec<CompiledAlt> = vec![(Vec::new(), vec![0])];
        for (i, expr) in exprs.into_iter().enumerate() {
            let is_last = (i + 1 == n) && tail_ctx;
            let choices: Vec<CompiledAlt> = match self.compile_slot(expr, parent, is_last)? {
                Slot::Fixed(syms) => {
                    let gaps = vec![0; syms.len() + 1];
                    vec![(syms, gaps)]
                }
                Slot::Nullable {
                    mut present,
                    absent_nones,
                } => {
                    // present-forms first, then the absent alternative (which
                    // contributes only its placeholder count).
                    present.push((Vec::new(), vec![absent_nones]));
                    present
                }
                // A distributed plain group: its arms fan out as-is, no ε arm.
                Slot::Choices(arms) => arms,
            };
            // Dedup the running product at **every** position rather than only at
            // the end (the trailing `retain` below). Without this, a chain of `k`
            // duplicate-arm inline groups (`(X|X) (X|X) … (X|X)`) materializes the
            // full `m^k` cartesian product before the final dedup collapses it to a
            // single alternative — a deterministic `2^k` build blowup (#404, H6-7).
            // First-occurrence dedup at each fold step produces the byte-identical
            // final set (same alternatives, same order, same `dedup_and_check_alts`
            // verdict), bounding the working set to the distinct alternatives at that
            // prefix length — exactly as Python Lark's `SimplifyRule_Visitor` dedups
            // each group's arms *before* the cross-product. This is the same technique
            // [`repeat_union`](Self::repeat_union) already applies on the `~n` repeat
            // path (#252); here it is wired into the general per-position loop.
            acc = Self::concat_alts_dedup(&acc, &choices);
            // Deterministic build-cost signal: the size of the running product
            // after each fold step. With the deduping fold this stays flat in the
            // group-chain length `k`; the old non-deduping fold made it `2^k`
            // (#404, gated by `tests/test_grammar_build_scaling.rs`).
            crate::perf::add_expansion_alts(acc.len() as u64);
        }
        // Distributing two optionals can coincide (`X? X?` → `X X | X | X | ε`);
        // identical alternatives would reduce/reduce on the same item, so keep the
        // first occurrence of each (Python's grammar dedups identical rules too). The
        // dedup is keyed on the **full** `(syms, gaps)` — including empty arms' maybe
        // `_EMPTY`-marker counts (gaps) — so it collapses only *byte-identical*
        // alternatives, exactly as Python's `SimplifyRule_Visitor.expansions` dedups
        // identical expansion **trees** (the `_EMPTY` markers are tree children there,
        // hence part of the key).
        //
        // Crucially, two empty arms that differ in their `_EMPTY` count are **kept
        // distinct** here — that provenance is what Python preserves through dedup so a
        // nested optional collides at the final rule build (H4-8, #351). The two
        // empty-producing operators carry different counts: `?` (Python's `EBNF_to_BNF.expr`)
        // distributes a **bare** ε (`[],[0]`), while `[...]` (Python's `EBNF_to_BNF.maybe`)
        // distributes `[_EMPTY] * FindRuleSize` (`[],[n]`). For `([A]?) B` / `[[A]?] B`
        // the inner `[A]`'s absent arm (1 `_EMPTY`) and the outer `?`'s bare ε (0) thus
        // stay as two distinct empties; once the tail `B` is concatenated they become
        // two distinct `start -> B` arms (gaps `[1,0]` vs `[0,0]`) that collide in
        // [`dedup_and_check_alts`](GrammarCompiler::dedup_and_check_alts) — Python's
        // "Rules defined twice", on every backend. Collapsing them on emptiness alone
        // (as before) destroyed the bare-vs-marker distinction *inside the group*,
        // before the tail could surface it, so the collision was silently lost.
        //
        // A *lone* nested optional (`([A]?)`, no tail) stays accepted: its two distinct
        // empties survive this dedup but reach `dedup_and_check_alts` still empty, where
        // duplicate **empty** rules are tolerated and collapsed on emptiness alone
        // (Python's "Rules defined twice" fires only for non-empty `dups[0].expansion`).
        // The first occurrence wins there, so under `maybe_placeholders` the kept arm is
        // the inner `[A]`'s `(True,)` None-bearing one (`""` → `[None]`), matching Python.
        let mut seen = std::collections::HashSet::new();
        acc.retain(|a| seen.insert(a.clone()));
        Ok(acc)
    }

    /// Cartesian concatenation of two alternative lists: every `prefix` alternative
    /// followed by every `suffix` alternative, merging the seam gap (the prefix's
    /// trailing `None` count plus the suffix's leading one). This is the per-position
    /// product `compile_expansion` runs; factored out so repeat-inlining
    /// ([`inline_repeat`](Self::inline_repeat)) reuses the exact same gap arithmetic.
    fn concat_alts(prefix: &[CompiledAlt], suffix: &[CompiledAlt]) -> Vec<CompiledAlt> {
        let mut next = Vec::with_capacity(prefix.len() * suffix.len());
        for (psyms, pgaps) in prefix {
            for (csyms, cgaps) in suffix {
                let mut syms = psyms.clone();
                syms.extend_from_slice(csyms);
                // Merge gap vectors: the seam gap is the sum of the prefix's
                // trailing gap and the suffix's leading gap.
                let mut gaps = pgaps[..pgaps.len() - 1].to_vec();
                gaps.push(pgaps[pgaps.len() - 1] + cgaps[0]);
                gaps.extend_from_slice(&cgaps[1..]);
                next.push((syms, gaps));
            }
        }
        next
    }

    /// [`concat_alts`](Self::concat_alts) followed by an immediate first-occurrence
    /// dedup of the product. Folding the dedup *into each step* is what keeps a
    /// repeated optional (`[X]~n`) from blowing up: `[X]` contributes a present and
    /// an absent arm, so the naive `n`-fold product is `2^n` alternatives before the
    /// trailing dedup collapses it to the `n+1` distinct counts — Python Lark has
    /// this same exponential in `_generate_repeats` (`[A]~15` already takes seconds
    /// before it raises "Rules defined twice"). Deduping per step bounds the working
    /// set to the distinct alternatives at that prefix length, so `[X]~n` stays
    /// linear-ish in `n` (capped at Python's `REPEAT_BREAK_THRESHOLD = 50`) while
    /// producing the byte-identical final set — same alternatives, same
    /// `dedup_and_check_alts` collision verdict, no `2^n` materialization (#252).
    fn concat_alts_dedup(prefix: &[CompiledAlt], suffix: &[CompiledAlt]) -> Vec<CompiledAlt> {
        let product = Self::concat_alts(prefix, suffix);
        let mut seen = std::collections::HashSet::new();
        product
            .into_iter()
            .filter(|a| seen.insert(a.clone()))
            .collect()
    }

    /// Python Lark's `EBNF_to_BNF._generate_repeats` small case (`mx < 50`,
    /// `REPEAT_BREAK_THRESHOLD`): a bounded `x~mn..mx` inlines into the parent
    /// expansion as one alternative per count `k` in `mn..=mx`, each the `k`-fold
    /// concatenation of the inner's present alternatives — with **no** helper rule
    /// (Python emits `expansions([expansion([rule]*k) …])`, distributed by
    /// `SimplifyRule_Visitor`). Returns `None` to fall back to the helper form
    /// ([`compile_repeat`](Self::compile_repeat)) when the inner carries an alias
    /// (inlining would lose the named subtree) or the max count reaches Python's
    /// break threshold (large repeats factor into sub-rules upstream; we keep the
    /// single-helper form, which is byte-identical in the tree).
    ///
    /// Inlining is what stops `"d"~1` from minting a `__anon_rep: D` helper that
    /// duplicates a sibling literal `D` alternative as an unresolvable reduce/reduce
    /// Python never reports (#176).
    fn inline_repeat(
        &mut self,
        inner: &Expr,
        mn: usize,
        mx: usize,
        parent: &str,
    ) -> Result<Option<Vec<CompiledAlt>>, GrammarError> {
        // Python Lark's load_grammar.REPEAT_BREAK_THRESHOLD.
        const REPEAT_BREAK_THRESHOLD: usize = 50;
        if mx >= REPEAT_BREAK_THRESHOLD || Self::expr_contains_alias(inner) {
            return Ok(None);
        }
        // #212: a `[X]~n` under `maybe_placeholders` must distribute each copy's
        // present + absent forms into the parent's alternatives, exactly as Python
        // Lark's `_generate_repeats` does (each `[X]` in the expansion distributes
        // via `SimplifyRule_Visitor`). Without this, `inner_alternatives` compiles
        // the `[X]` into a helper symbol that absorbs the distribution, so the
        // parent sees only one alternative (`helper helper …`) and the placeholder-
        // position collision that Python's "Rules defined twice" catches is hidden.
        if let Expr::Maybe(alts) = inner {
            // A `[X]~n` distributes each copy as present + absent forms, exactly as
            // Python Lark's `_generate_repeats` does (`[X]~2` ≡ `[X] [X]`). The
            // absent arm carries `[_EMPTY] * FindRuleSize` positional markers
            // *regardless* of `maybe_placeholders` (Python's `maybe`), so its
            // present/absent collapse onto a sibling reaches `dedup_and_check_alts`
            // — the colliding `[A]~2 C` rejection Python raises under both modes
            // (#252). Without `maybe_placeholders` the markers are stripped before
            // tree output (no `None` children) at the final rule build below; with
            // it they become positional `None`s (#212). The non-distributable
            // (aliased) inner still falls back to the helper form.
            let present = match self.distributable_alternatives(alts.clone(), parent)? {
                Some(p) => p,
                None => return Ok(None), // aliased — fall back to helper form
            };
            let absent_nones = self.maybe_absent_size(&present);
            // Each copy is the present alternatives plus one absent
            // alternative (empty, contributing `absent_nones` placeholders) —
            // the same shape `compile_expansion` builds from a `Nullable` slot.
            let mut per_copy = present;
            per_copy.push((Vec::new(), vec![absent_nones]));
            return Ok(Some(Self::repeat_union(&per_copy, mn, mx, parent)?));
        }
        // One copy's present alternatives — a non-aliased group fans its arms out,
        // a plain atom is a single arm. Compiled once and replicated, exactly as
        // Python reuses the same `rule` subtree for each of the `[rule]*k` copies.
        let base = self.inner_alternatives(inner, parent)?;
        // Union over each count k of the k-fold cartesian concatenation of `base`
        // (k == 0 → the single empty alternative), with `x~0..1` ≡ `x?` ≡ `x | ε`.
        Ok(Some(Self::repeat_union(&base, mn, mx, parent)?))
    }

    /// The deduped union over `k` in `mn..=mx` of the `k`-fold cartesian
    /// concatenation of `base` — the inlined alternatives of a bounded `x~mn..mx`.
    /// Built **incrementally**: the `k`-fold product is one deduping concat of the
    /// `(k-1)`-fold product with `base`, and each count's running product is folded
    /// into the output as it is reached.
    ///
    /// Two short-circuits keep a repeated optional from the `2^k` blow-up (#252):
    ///
    ///  1. Per-step dedup ([`concat_alts_dedup`](Self::concat_alts_dedup)) collapses
    ///     byte-identical alternatives at every prefix length rather than only at the
    ///     end.
    ///  2. **Early collision detection.** A `[X]`-style `base` carries an absent arm
    ///     whose positional `_EMPTY` markers (gaps) differ from a present arm of the
    ///     same kept symbols, so the running product accumulates alternatives with
    ///     identical `syms` but distinct gaps — the exact collision
    ///     [`dedup_and_check_alts`](GrammarCompiler::dedup_and_check_alts) rejects as
    ///     "Rules defined twice" (a colliding `[X]~n`, e.g. `[A]~2 C`). That verdict
    ///     is independent of the surrounding context (differing gaps survive
    ///     concatenation with any tail), so we raise it on the *first* step that
    ///     produces such a pair instead of materializing the full product. Python
    ///     Lark reaches the same rejection but only after the exponential
    ///     `_generate_repeats` expansion (`[A]~15` already takes seconds); matching
    ///     its verdict without its blow-up is a pure efficiency win, not a divergence.
    fn repeat_union(
        base: &[CompiledAlt],
        mn: usize,
        mx: usize,
        origin: &str,
    ) -> Result<Vec<CompiledAlt>, GrammarError> {
        let mut out: Vec<CompiledAlt> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Running `k`-fold product, grown one factor at a time (k == 0 is the lone
        // empty alternative). `concat_alts_dedup` keeps it at its distinct size.
        let mut acc: Vec<CompiledAlt> = vec![(Vec::new(), vec![0])];
        for k in 0..=mx {
            if k > 0 {
                acc = Self::concat_alts_dedup(&acc, base);
                // Two distinct deduped alternatives sharing a `syms` sequence differ
                // only in gaps → they will collide in `dedup_and_check_alts`. Raise
                // now (before the next product step) rather than expanding further.
                let mut syms_seen: std::collections::HashSet<&Vec<Symbol>> =
                    std::collections::HashSet::new();
                for (syms, _) in &acc {
                    if !syms.is_empty() && !syms_seen.insert(syms) {
                        let rhs: Vec<&str> = syms.iter().map(|s| s.name()).collect();
                        return Err(GrammarError::Other {
                            msg: format!(
                                "Rules defined twice: {origin} -> {} \
                                 (Might happen due to colliding expansion of optionals: [] or ?)",
                                rhs.join(" ")
                            ),
                        });
                    }
                }
            }
            if k >= mn {
                for a in &acc {
                    if seen.insert(a.clone()) {
                        out.push(a.clone());
                    }
                }
            }
        }
        Ok(out)
    }

    /// Compile one position of an expansion into either a single fixed symbol
    /// sequence (the common case) or, for a distributable nullable, the set of
    /// present-form alternatives to fan out across the parent (see
    /// [`compile_expansion`](Self::compile_expansion)). Every nullable — leading or
    /// trailing, `X?` / `X*` / `[X]` — is distributed exactly like Python Lark's
    /// `SimplifyRule_Visitor`: the empty case goes into the parent's alternatives.
    /// A trailing `X*` no longer keeps a `__star: __plus | ε` wrapper (#91/#32); its
    /// ε now lives in the parent, distinguished by parent context, so the duplicate-ε
    /// reduce/reduce the wrapper used to dodge cannot arise (two `*` in different
    /// parents distribute ε onto different origins). `is_last` is therefore unused
    /// here — distribution is position-independent.
    fn compile_slot(
        &mut self,
        expr: Expr,
        parent: &str,
        is_last: bool,
    ) -> Result<Slot, GrammarError> {
        // A plain `(a|b)` group distributes into the parent at *every* position
        // (Python never gives an inline group a helper rule — see
        // `Slot::Choices`) unless it carries an alias (an alias names a subtree
        // that inline distribution would lose, so those fall back to the helper
        // form).
        if let Expr::Group(alts) = &expr {
            if !Self::expr_contains_alias(&expr) {
                if let Some(arms) = self.distributable_alternatives(alts.clone(), parent)? {
                    return Ok(Slot::Choices(arms));
                }
            }
        }
        // Every nullable — leading *or* trailing, `X?` / `X*` / `[X]` — distributes
        // exactly like Python Lark's `SimplifyRule_Visitor`: the empty case is
        // pushed into the parent's alternatives and the present case is the shared
        // recurse helper (for `*`) or the inner symbol(s). The trailing `*` no
        // longer keeps a `__star: __plus | ε` wrapper — its ε now lives in the
        // parent, distinguished by parent context, which is both structurally
        // faithful and free of the duplicate-ε R/R the wrapper used to dodge
        // (#91/#32). `is_last` is unused on this path now; the `*`/`?`/`[…]`
        // distribution is position-independent. `try_distribute` never compiles
        // anything on its `None` path, so the fall-through `compile_expr` below
        // compiles the position exactly once.
        let _ = is_last;
        if !Self::expr_contains_alias(&expr) {
            if let Some(slot) = self.try_distribute(&expr, parent)? {
                return Ok(slot);
            }
        }
        // A bounded `~n` / `~n..m` (the `?`/`*`/`+` operators were consumed by
        // `try_distribute` / the group check above; only `~`-repeats with a finite
        // `max` reach here — including `~0..1`, which `try_distribute` no longer
        // intercepts as a `?`) inlines into the parent's alternatives rather than
        // minting a helper rule, matching Python's `_generate_repeats` (#176/#258).
        if let Expr::Repeat {
            inner,
            min,
            max: Some(max),
            ..
        } = &expr
        {
            if let Some(arms) = self.inline_repeat(inner, *min, *max, parent)? {
                return Ok(Slot::Choices(arms));
            }
        }
        Ok(Slot::Fixed(vec![self.compile_expr(expr, parent)?]))
    }

    /// If `expr` is a distributable leading nullable (`X?`, `X*`, or a `[X]`),
    /// return its distribution slot (present-form alternatives + the absent
    /// case's `None` count); otherwise `None`. The `None` paths bail *before*
    /// compiling anything, so the caller may compile the expr afresh without
    /// emitting duplicate helper rules.
    fn try_distribute(&mut self, expr: &Expr, parent: &str) -> Result<Option<Slot>, GrammarError> {
        match expr {
            // `X?` / `(...)?` → present forms of the inner. The `?` *operator* is
            // Python's `maybe()`: when the inner is itself a `[Y]`, its absent arm
            // inherits the inner's `None` placeholder (`([A])?` → `""` is `[None]`).
            // A `~0..1` repeat shares this `(min: 0, max: Some(1))` shape but is *not*
            // a `maybe` — its `k == 0` count is a pristine empty (`[A]~0..1` → `""` is
            // `[]`), so it routes to `inline_repeat` instead (gated on `kind: Op`).
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
                kind: RepeatKind::Op,
            } => Ok(self
                .present_forms((**inner).clone(), parent)?
                .map(|present| Slot::Nullable {
                    present,
                    absent_nones: 0,
                })),
            // `X*` → the shared one-or-more recurse helper (inner arms inlined),
            // with the empty case distributed into the parent — exactly Python's
            // `EBNF_to_BNF` (`a: b c* d` → `_c: <recurse>` + `a: b _c d | b d`).
            Expr::Repeat {
                inner,
                min: 0,
                max: None,
                kind: RepeatKind::Op,
            } => {
                let arms = self.inner_alternatives(inner, parent)?;
                let plus = self.recurse_helper_keyed(arms, &inner.python_recurse_key());
                Ok(Some(Slot::Nullable {
                    present: vec![(vec![plus], vec![0, 0])],
                    absent_nones: 0,
                }))
            }
            // `[X]`: distributed like Python's `maybe()` → `expansions(X, _EMPTY*n)`.
            // Under `maybe_placeholders` the absent alternative contributes the
            // widest present form's kept-slot count as positional `None`
            // placeholders (Python's `_EMPTY` markers → `empty_indices`); without
            // placeholders it contributes nothing.
            Expr::Maybe(alts) => {
                let present = match self.distributable_alternatives(alts.clone(), parent)? {
                    Some(p) => p,
                    None => return Ok(None),
                };
                Ok(Some(Slot::Nullable {
                    present: present.clone(),
                    absent_nones: self.maybe_absent_size(&present),
                }))
            }
            _ => Ok(None),
        }
    }

    /// The non-empty ("present") derivations of an expr, used when distributing a
    /// leading nullable. Returns `None` only when the inner carries an alias (its
    /// named subtree must survive a helper), so the caller keeps the helper.
    /// (A `[X]` standing directly at a rule position distributes via
    /// `try_distribute`'s own `Maybe` arm, placeholders and all.)
    fn present_forms(
        &mut self,
        expr: Expr,
        parent: &str,
    ) -> Result<Option<Vec<CompiledAlt>>, GrammarError> {
        let single = |sym: Symbol| Some(vec![(vec![sym], vec![0, 0])]);
        match expr {
            Expr::Value(v) => Ok(single(self.compile_value(v, parent)?)),
            Expr::Group(alts) => self.distributable_alternatives(alts, parent),
            // A `[X]` nested under an outer `?` (`([X])?`) distributes the inner
            // maybe's own present forms *plus* its absent arm carrying the positional
            // `_EMPTY` markers (the same `[_EMPTY] * FindRuleSize` Python's `maybe`
            // always emits), then the outer `?` re-adds a bare ε in
            // `compile_expansion`. This holds in *both* modes (#258/#252):
            //   - When the inner-absent and outer-ε arms coincide as duplicate
            //     *empty* productions (a lone `([A])?`), `compile_expansion`'s
            //     empty-arm dedup collapses them — keeping the first, None-bearing
            //     arm under `maybe_placeholders` (Python's `(True,)`), or to a bare
            //     ε without — so a non-colliding nullable builds instead of minting a
            //     spurious second empty production (the #258 LALR conflict).
            //   - When a tail follows (`([A])? C`), the inner-absent arm surfaces its
            //     markers onto the tail (`C` with a leading None) and collides with
            //     the outer-ε `C` in `dedup_and_check_alts` — the rejection Python
            //     raises in both modes (#252).
            Expr::Maybe(alts) => {
                let mut present = match self.distributable_alternatives(alts, parent)? {
                    Some(p) => p,
                    None => return Ok(None),
                };
                let absent_nones = self.maybe_absent_size(&present);
                present.push((Vec::new(), vec![absent_nones]));
                Ok(Some(present))
            }
            // A nested `?` operator collapses: `(X?)?` ≡ `X?`, so drop the inner
            // optionality and let the outer distribution re-add the single ε. A
            // `~0..1` repeat (`kind: Tilde`) is *not* collapsible this way — its
            // `k == 0` count is a placeholder-free empty — so it falls through to the
            // `~n..m` arm below and inlines via `inline_repeat`.
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
                kind: RepeatKind::Op,
            } => self.present_forms(*inner, parent),
            // `X*` / `X+` present form is the shared one-or-more recurse helper
            // (inner arms inlined, Python's `EBNF_to_BNF`).
            Expr::Repeat {
                inner,
                min: 0,
                max: None,
                ..
            }
            | Expr::Repeat {
                inner,
                min: 1,
                max: None,
                ..
            } => {
                let arms = self.inner_alternatives(&inner, parent)?;
                let plus = self.recurse_helper_keyed(arms, &inner.python_recurse_key());
                Ok(single(plus))
            }
            // Bounded `~n` / `~n..m` (including `~0..1`) nested under a distributed
            // `?` inlines its counts as present forms too (the directly-positioned
            // case is handled in `compile_slot`); large/aliased repeats fall back to
            // the helper.
            Expr::Repeat {
                inner,
                min,
                max: Some(max),
                ..
            } => match self.inline_repeat(&inner, min, max, parent)? {
                Some(arms) => Ok(Some(arms)),
                None => Ok(single(self.compile_repeat(
                    *inner,
                    min,
                    Some(max),
                    parent,
                )?)),
            },
            // Exact / bounded repetition: a single helper symbol.
            other => Ok(single(self.compile_expr(other, parent)?)),
        }
    }

    /// Lower each alternative of a group/`[...]` into distributed present-form
    /// sequences, flattened into one alternative list. Returns `None` if any
    /// alternative carries an alias (inline distribution would lose the named
    /// subtree), so the caller falls back to the helper form.
    fn distributable_alternatives(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
    ) -> Result<Option<Vec<CompiledAlt>>, GrammarError> {
        if alts.iter().any(|a| a.alias.is_some()) {
            return Ok(None);
        }
        let mut out = Vec::new();
        for alt in alts {
            // `tail_ctx: false` — these symbols are spliced mid-rule into the
            // parent (the distributed nullable is never final), so a trailing
            // nullable here is not actually trailing and must distribute too.
            let subs = self.compile_expansion(alt.expansion, parent, false)?;
            out.extend(subs);
        }
        Ok(Some(out))
    }

    /// Whether an expr (recursively) carries a `->` alias on any of its grouped
    /// alternatives. A distributable nullable wrapping an alias is kept as a
    /// helper instead, so the alias's named subtree survives.
    fn expr_contains_alias(expr: &Expr) -> bool {
        match expr {
            Expr::Value(_) => false,
            Expr::Repeat { inner, .. } => Self::expr_contains_alias(inner),
            Expr::Group(alts) | Expr::Maybe(alts) => alts
                .iter()
                .any(|a| a.alias.is_some() || a.expansion.iter().any(Self::expr_contains_alias)),
        }
    }

    fn compile_expr(&mut self, expr: Expr, parent: &str) -> Result<Symbol, GrammarError> {
        match expr {
            Expr::Value(v) => self.compile_value(v, parent),
            Expr::Group(alts) => self.compile_group(alts, parent, false),
            Expr::Maybe(alts) => self.compile_maybe(alts, parent),
            Expr::Repeat {
                inner, min, max, ..
            } => self.compile_repeat(*inner, min, max, parent),
        }
    }

    fn compile_value(&mut self, v: Value, parent: &str) -> Result<Symbol, GrammarError> {
        match v {
            // A named terminal reference is filtered iff `_`-prefixed (Lark's
            // `Terminal(s, filter_out=s.startswith('_'))`).
            Value::Terminal(name) => {
                let filter_out = name.starts_with('_');
                Ok(Symbol::Terminal(Terminal { name, filter_out }))
            }
            Value::Rule(name) => Ok(Symbol::NonTerminal(NonTerminal::new(name))),
            Value::Literal(lit) => {
                // An anonymous *string* literal is filtered out of the tree
                // (keyword-like punctuation); an anonymous *regex* literal is kept,
                // matching Python Lark. This is a property of the *occurrence*, not
                // the terminal — the same terminal may be kept elsewhere.
                let filter_out = matches!(lit, LiteralVal::Str(..));
                let term_name = self.get_or_create_terminal(lit)?;
                Ok(Symbol::Terminal(Terminal {
                    name: term_name,
                    filter_out,
                }))
            }
            Value::Range(from, to) => {
                let pat_str = format!("[{}-{}]", regex::escape(&from), regex::escape(&to));
                let pat = Pattern::Re(PatternRe::new(&pat_str, 0)?);
                // A char-range terminal is a regex literal — kept, like `/[a-z]/`.
                let name = self.intern_anon_pattern(pat, None, false);
                Ok(Symbol::Terminal(Terminal {
                    name,
                    filter_out: false,
                }))
            }
            Value::TemplateUsage { name, args } => self.instantiate_template(&name, args, parent),
        }
    }

    fn compile_group(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
        optional: bool,
    ) -> Result<Symbol, GrammarError> {
        // Lower every alternative up front so the structural cache key is built
        // from the compiled symbols, then share or emit one helper for it. A
        // single source alternative may itself distribute into several (a leading
        // nullable fanned out), so each contributes one *or more* compiled
        // alternatives. (The `parent` name is inert below the top level — only
        // template usage reads it, and that path ignores it — so lowering before
        // the helper is named is behaviourally identical to the old numbering.)
        // Same dedup + collision check as a named rule's alternatives: Python
        // inlines groups into the parent, where its `expansions` dedup and
        // "Rules defined twice" check run — so `(X | X)` collapses (and then
        // takes the single-symbol shortcut below, like Python's inlined `X`),
        // and `([A] [A] B)` is rejected at load. A group's inner alias is the
        // legitimate helper-naming case, so nested aliases are *not* re-rejected
        // here (the enclosing rule already ran that check).
        let compiled = self.compile_alternatives(alts, parent, false)?;
        // A plain single non-aliased alternative that compiles to exactly one
        // symbol *is* that symbol — skip the wrapper rule. Besides dropping a
        // redundant transparent node, this lets `(X)+` share `X`'s recurse helper
        // and stops `("A"?)?` from stacking a second nullable rule.
        if !optional
            && compiled.len() == 1
            && compiled[0].1.is_none()
            && compiled[0].0 .0.len() == 1
            && compiled[0].0 .1.iter().all(|&g| g == 0)
        {
            return Ok(compiled
                .into_iter()
                .next()
                .unwrap()
                .0
                 .0
                .into_iter()
                .next()
                .unwrap());
        }
        let kind = if optional {
            HelperKind::GroupOptional
        } else {
            HelperKind::Group
        };
        Ok(self.intern_helper(kind, compiled))
    }

    /// Share or emit the anonymous helper rule(s) for `kind` over its already
    /// lowered alternatives. On a structural cache hit the existing helper
    /// non-terminal is returned and nothing is emitted; otherwise a fresh
    /// `__anon_*` rule set is generated, its inlined size recorded (Lark's
    /// `FindRuleSize`), and the name cached under its [`HelperKey`]. This is the
    /// single choke point that extends Python Lark's `rules_cache` to every EBNF
    /// helper, so repeated `(",", X)*`-style patterns collapse to one rule
    /// instead of colliding as duplicate nullable helpers under LALR.
    fn intern_helper(
        &mut self,
        kind: HelperKind,
        alts: Vec<(CompiledAlt, Option<String>)>,
    ) -> Symbol {
        // What to share is anchored to Python Lark's `rules_cache`. Python caches
        // only the *non-nullable* recurse core (`_c: _c c | c`, keyed on the inner
        // expression) — shared by both `+` and `*` — and has *no* nullable `*` rule
        // at all: `SimplifyRule_Visitor` distributes `c*`'s empty case into each
        // parent (`a: b c* d` → `_c: _c c | c` + `a: b _c d | b d`). After #91 the
        // [`recurse_helper`](Self::recurse_helper) does exactly that, and `*`
        // distributes its ε into the parent via `compile_slot`/`try_distribute`, so
        // a `Star` wrapper reaches `intern_helper` only on the rare fallback where a
        // `*` is nested where a *single symbol* is mandatory (inside `~n`).
        //
        //   * `Group` / `Star` — share. Sharing the `(",", X)` group lets the
        //     `recurse_cache` share its `+`-recurse `__plus` in turn (keyed on the
        //     inner arms). Two byte-identical fallback `Star` wrappers would collide
        //     as an unresolvable reduce/reduce the moment they reduce ε on the same
        //     lookahead in a common state; sharing recognizes they are one rule. It
        //     does not over-narrow — the collision is proof the parser already
        //     cannot tell the wrappers apart (they merge via the shared `__plus`,
        //     like Python's shared `_c`), so unifying widens no contextual scanner.
        //   * `Opt` / `Maybe` / `GroupOptional` — do *not* share. These are the
        //     `?`/`[...]` helpers Python inlines into parents. Unlike the `*`
        //     wrapper there is no pre-shared core forcing their states together, so
        //     sharing one *forces* a merge LALR would otherwise keep separate —
        //     unioning two parents' follow-sets into a contextual scanner that LALR
        //     never actually merges, silently widening it (it made `csv.lark`'s
        //     `header` start trying `row`'s terminals, picking the higher-priority
        //     `NON_SEPARATOR_STRING` over `WORD`). Leaving them per-parent keeps
        //     lark-rs byte-identical to the oracle, which never shares them either.
        //
        // #97 distributed *leading* (non-final) `*`/`?`/`[...]` into the parent;
        // #91 completed the convergence — *trailing* `*`/`?` now distribute too
        // (`(A|B)+`'s arms inline straight into the recurse rule), so the only
        // nullables that still reach `intern_helper` are the `?`/`[...]` per-parent
        // helpers and the `~n`-nested `Star` fallback above.
        let cacheable = matches!(kind, HelperKind::Group | HelperKind::Star);
        let key: HelperKey = (kind.clone(), self.current_keep_all, alts.clone());
        if cacheable {
            if let Some(name) = self.helper_cache.get(&key) {
                return Symbol::NonTerminal(NonTerminal::new(name));
            }
        }
        let tag = match kind {
            HelperKind::Group | HelperKind::GroupOptional => AnonKind::Group,
            HelperKind::Maybe => AnonKind::Maybe,
            HelperKind::Opt => AnonKind::Opt,
            HelperKind::Star => AnonKind::Star,
        };
        let name = self.fresh_anon_rule(tag);
        let origin = NonTerminal::new(&name);
        // A nested bare nullable (`[[A]]`, `([A])` under `[...]`) distributes its
        // own absent arm as an *empty* alternative in `alts`; the synthetic empty
        // arm an optional helper appends below would then duplicate it, minting two
        // byte-identical `helper -> ε` productions — the self reduce/reduce Python
        // never reports (#401, H6-4). Python's `EBNF_to_BNF` collapses the twin
        // empties (the inner `maybe()` and the outer `[...]` empty are one ε arm), so
        // skip the synthetic empty when `alts` already carries one. The pre-existing
        // arm keeps its own placeholder gaps (first-occurrence wins, matching
        // `dedup_and_check_alts`'s empty-arm dedup).
        let has_empty_arm = alts.iter().any(|((syms, _), _)| syms.is_empty());
        let mut max_size = 0;
        for (order, ((syms, gaps), alias)) in alts.iter().enumerate() {
            // An alternative's inlined size counts its kept symbols plus any
            // `None`s its distributed nested maybes left inline, so nested
            // placeholders compose (Lark's `FindRuleSize`).
            let size: usize = syms.iter().map(|s| self.symbol_size(s)).sum::<usize>()
                + gaps.iter().sum::<usize>();
            max_size = max_size.max(size);
            let options = RuleOptions {
                nones_before: self.stored_output_gaps(gaps.clone()),
                ..self.anon_opts()
            };
            self.rules.push(Rule::new(
                origin.clone(),
                syms.clone(),
                alias.clone(),
                options,
                order,
            ));
        }
        // `*` helpers stay size 0 (transparent, inlined away) — `symbol_size` of
        // their lone `+`-recurse child is already 0, so recording `max_size` here
        // is a no-op for them and keeps the bookkeeping uniform.
        self.helper_sizes.insert(name.clone(), max_size);
        self.emit_helper_empty_arm(&kind, &origin, &name, has_empty_arm, max_size);
        if cacheable {
            self.helper_cache.insert(key, name);
        }
        Symbol::NonTerminal(origin)
    }

    /// Emit the kind-specific *empty / nullable* arm of an anonymous helper, after
    /// its present alternatives have already been pushed by
    /// [`intern_helper`](Self::intern_helper). Each `HelperKind` differs only in
    /// whether (and how) it produces an ε production:
    ///
    ///   * `Group` — `(...)` spliced inline, no empty arm.
    ///   * `GroupOptional` — a placeholder-less optional: a bare empty arm.
    ///   * `Maybe` — `[...]` under `maybe_placeholders`: an empty arm emitting one
    ///     `None` per kept slot of the widest alternative (`max_size`).
    ///   * `Opt` / `Star` — `x?` / `x*`: a single-arm nullable wrapper `P: inner | ε`.
    ///
    /// `GroupOptional` / `Maybe` skip the synthetic empty when `has_empty_arm` (a
    /// nested bare nullable already distributed one — #401, H6-4), keeping that first
    /// arm's own placeholder gaps rather than minting a second byte-identical `ε`
    /// production (the self reduce/reduce Python never reports).
    fn emit_helper_empty_arm(
        &mut self,
        kind: &HelperKind,
        origin: &NonTerminal,
        name: &str,
        has_empty_arm: bool,
        max_size: usize,
    ) {
        match kind {
            // `(...)` is spliced inline with no empty arm.
            HelperKind::Group => {}
            // A placeholder-less optional group: just an empty alternative
            // (unless a nested nullable already distributed one — #401, H6-4).
            HelperKind::GroupOptional if !has_empty_arm => {
                self.rules.push(Rule::new(
                    origin.clone(),
                    vec![],
                    None,
                    self.anon_opts(),
                    100,
                ));
            }
            HelperKind::GroupOptional => {}
            // `[...]` under maybe_placeholders: the empty case emits one `None`
            // per kept slot of the widest alternative (skipped when a nested
            // nullable already supplied the empty arm — #401, H6-4).
            HelperKind::Maybe if !has_empty_arm => {
                let empty_opts = RuleOptions {
                    placeholder_count: max_size,
                    ..self.anon_opts()
                };
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, empty_opts, 100));
            }
            HelperKind::Maybe => {}
            // `x?` / `x*`: a single-arm nullable wrapper `P: inner | ε`.
            HelperKind::Opt | HelperKind::Star => {
                self.nullable_opts.insert(name.to_string());
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, self.anon_opts(), 1));
            }
        }
    }

    fn compile_maybe(
        &mut self,
        alts: Vec<AliasedExpansion>,
        parent: &str,
    ) -> Result<Symbol, GrammarError> {
        // Without maybe_placeholders, `[x]` is just an optional group.
        if !self.maybe_placeholders {
            return self.compile_group(alts, parent, true);
        }
        // With maybe_placeholders, the empty case emits one `None` per kept symbol,
        // using the widest alternative (Python Lark inserts max-width placeholders).
        // A kept slot is a kept token *or* the inlined size of a nested maybe/group,
        // so nested optionals compose (Lark `FindRuleSize`); `intern_helper` records
        // the widest alternative's size and threads it into the empty production.
        // Same dedup + collision check as a named rule's alternatives (Python
        // distributes `[...]` into the parent, where they run; see
        // `dedup_and_check_alts`). Nested aliases are not re-rejected here (the
        // enclosing rule already ran that check, and a group's inner alias is the
        // legitimate helper-naming case).
        let compiled = self.compile_alternatives(alts, parent, false)?;
        Ok(self.intern_helper(HelperKind::Maybe, compiled))
    }

    /// Number of tree children a symbol contributes to an absent `[...]`'s `None`
    /// placeholder count — Python Lark's `FindRuleSize`. A kept token is 1, a
    /// filtered token 0; a named rule is 1, a transparent `_rule` / `*` / `+` / `~`
    /// helper is 0 (inlined-away, like Lark's `_`-prefixed symbols); a nested
    /// maybe / optional / group contributes its own recorded inlined size, so
    /// placeholders compose through arbitrary nesting.
    fn symbol_size(&self, s: &Symbol) -> usize {
        match s {
            Symbol::Terminal(t) => {
                if self.current_keep_all {
                    1
                } else if t.filter_out {
                    0
                } else {
                    1
                }
            }
            Symbol::NonTerminal(nt) => {
                if let Some(&size) = self.helper_sizes.get(&nt.name) {
                    size
                } else if nt.name.starts_with('_') {
                    0
                } else {
                    1
                }
            }
        }
    }

    /// The `_EMPTY`-marker count an absent `[...]` contributes, i.e. the widest
    /// present alternative's inlined size — Python Lark's `FindRuleSize`. This is
    /// computed **independently of `maybe_placeholders`** (Python's `maybe` always
    /// emits `[_EMPTY] * rule_size`): the markers give the absent arm its positional
    /// identity, so two colliding `[X]` optionals are caught by
    /// [`dedup_and_check_alts`](GrammarCompiler::dedup_and_check_alts) in *both*
    /// modes (#252). When `maybe_placeholders` is off the markers are stripped from
    /// the stored rule's gap vector before tree output (no `None` children), via
    /// [`stored_output_gaps`](Self::stored_output_gaps).
    fn maybe_absent_size(&self, present: &[CompiledAlt]) -> usize {
        present
            .iter()
            .map(|(syms, gaps)| {
                syms.iter().map(|s| self.symbol_size(s)).sum::<usize>() + gaps.iter().sum::<usize>()
            })
            .max()
            .unwrap_or(0)
    }

    /// Gap vector to *store* on a finished rule, given a compile-time gap vector
    /// that always carries the maybe `_EMPTY` markers. Under `maybe_placeholders`
    /// the markers become positional `None` children, so the gaps are kept (via
    /// [`stored_gaps`](GrammarCompiler::stored_gaps)); without it Python emits no
    /// placeholders, so the gaps are dropped — the markers existed only to drive the
    /// `dedup_and_check_alts` collision check, which has already run by this point
    /// (#252).
    pub(super) fn stored_output_gaps(&self, gaps: Vec<usize>) -> Vec<usize> {
        if self.maybe_placeholders {
            Self::stored_gaps(gaps)
        } else {
            Vec::new()
        }
    }

    /// The compiled inner alternatives of a `+`/`*` repetition, used to build the
    /// shared recurse helper. Python Lark's `EBNF_to_BNF` inlines a grouped
    /// repetition's arms directly into the recurse rule (`(A | B)+` →
    /// `_p: A | B | _p A | _p B`), so the recurse helper is keyed on, and built
    /// from, the *cartesian-expanded alternatives* of the inner expression — not a
    /// single nested group-helper symbol. A plain single-symbol inner is just one
    /// one-symbol arm; a non-aliased group fans out (and may itself distribute a
    /// nested leading nullable). An inner that carries an alias keeps the helper
    /// form (its named subtree must survive), so it falls through to `compile_expr`
    /// and lowers to a single arm over the group helper symbol.
    fn inner_alternatives(
        &mut self,
        inner: &Expr,
        parent: &str,
    ) -> Result<Vec<CompiledAlt>, GrammarError> {
        // A non-aliased group inlines its arms; everything else (including an
        // aliased group) becomes a single arm via `compile_expr`. `tail_ctx: false`
        // — the arms are spliced mid-rule into the recurse rule, so a trailing
        // nullable inside an arm is not actually trailing and must distribute too
        // (mirrors `distributable_alternatives`).
        if let Expr::Group(alts) = inner {
            if !Self::expr_contains_alias(inner) {
                let mut out = Vec::new();
                for alt in alts.clone() {
                    out.extend(self.compile_expansion(alt.expansion, parent, false)?);
                }
                return Ok(out);
            }
        }
        Ok(vec![(
            vec![self.compile_expr(inner.clone(), parent)?],
            vec![0, 0],
        )])
    }

    /// The shared one-or-more recurse helper for the inner `arms`, inlined exactly
    /// as Python Lark's `EBNF_to_BNF` does: for arms `[a0, a1, …]` the helper is
    /// `P: a0 | a1 | … | P a0 | P a1 | …` — every base arm (orders `0..k`) followed
    /// by every recurse arm `P ai` (orders `k..2k`). Cached by `(arms, keep_all)`
    /// so identical `x+`/`x*` occurrences reuse one rule (Python's `rules_cache`).
    /// Sharing collapses what would otherwise be duplicate, conflicting recurse
    /// rules into one, keeping `a+ b | a+` LALR.
    ///
    /// Inlining the arms (rather than nesting a `(A|B)` group helper under a
    /// single-symbol `P: g | P g`) is the structural fix for #91/#32: it makes the
    /// last symbol of the recursion a **terminal** built during the scan — matching
    /// Python — instead of a nonterminal group node the dynamic lexer's LIFO
    /// completer reverses, so the `dynamic_complete` resolve order falls out of
    /// `rule.order` + insertion order with no `sorted_families` split-point
    /// heuristic.
    ///
    /// When every arm is a single **named terminal** (`filter_out=false`),
    /// `keep_all` is irrelevant to token filtering (named terminals are always kept
    /// regardless) and is normalized to `false` in the cache key. This makes e.g.
    /// `DECIMAL+` in a `!float_lit` and `DECIMAL+` in a plain `int_lit` share one
    /// helper rule, matching Python Lark's grammar-wide `rules_cache` key (the inner
    /// expression only, no `keep_all` context). Without this normalization, two
    /// separate helpers with identical bodies cause an unresolvable LALR
    /// reduce/reduce conflict.
    ///
    /// `ast_key` is the inner expression's source-AST structural key
    /// (`Expr::python_recurse_key`). The share/split decision is owned by
    /// [`AuditShadow`](super::audit::AuditShadow): in the audit shadow pass (RC7/#272)
    /// the cache is keyed on `ast_key` so it matches Python Lark's
    /// `EBNF_to_BNF._add_recurse_rule` (which keys on the inner `expr` Tree) instead
    /// of the compiled arms; in the normal pass the load-bearing compiled-arms sharing
    /// (ADR-0013) is preserved verbatim and `ast_key` only records the over-share
    /// evidence the loader uses to decide whether to build the audit shadow at all.
    fn recurse_helper_keyed(&mut self, mut arms: Vec<CompiledAlt>, ast_key: &str) -> Symbol {
        // Dedup identical arms (first occurrence wins, order preserved). Python
        // Lark's `EBNF_to_BNF` builds the one-or-more rule from the *set* of inner
        // expansions, so `("b" | "b")*` collapses to a single recurse arm. Without
        // this, two byte-identical arms emit two identical base reductions (and two
        // identical `P arm` recurse reductions) into the same state — an
        // unresolvable reduce/reduce Python never reports (#210, seed 99). Order is
        // preserved because `rule.order` drives the resolve disambiguation (#49/#72).
        //
        // The dedup key is **filter-out-agnostic** (`sym_key`), mirroring Python's
        // `Symbol.__eq__` (which ignores `filter_out`): `(A | "a")+` with `A: "a"`
        // unifies the literal onto `A`, so its two inner arms `[A(keep)]` and
        // `[A(drop)]` differ only in `filter_out` and collapse to a single recurse
        // arm — keeping the first occurrence (and thus its `filter_out`). Without
        // this they emit two byte-identical `_p -> A` reductions, the spurious LALR
        // reduce/reduce (and Earley `_ambig` over-count) of #347 in the `+`/`*`
        // path, adjacent to the H4-9 top-level-alternation case.
        {
            let mut seen: std::collections::HashSet<(Vec<(bool, String)>, Vec<usize>)> =
                std::collections::HashSet::new();
            arms.retain(|(syms, gaps)| {
                let key = (
                    syms.iter()
                        .map(GrammarCompiler::sym_key)
                        .collect::<Vec<_>>(),
                    gaps.clone(),
                );
                seen.insert(key)
            });
        }
        // Named (non-filtered) single-terminal arms are always kept regardless of
        // keep_all, so the rule options difference is semantically invisible →
        // normalize the cache key (the common `WORD+` / `DECIMAL+` case).
        let all_named_terms = arms.iter().all(|(syms, gaps)| {
            syms.len() == 1
                && gaps.iter().all(|&g| g == 0)
                && matches!(&syms[0], Symbol::Terminal(t) if !t.filter_out)
        });
        let effective_keep_all = if all_named_terms {
            false
        } else {
            self.current_keep_all
        };
        // Audit shadow (RC7/#272): the [`AuditShadow`] owns the share/split decision.
        // In the shadow pass it keys on the inner-AST structural key so the verdict
        // matches Python Lark's `_add_recurse_rule` (`rules_cache[expr]`), reproducing
        // the un-shared helper split; in the real pass it observes the load-bearing
        // compiled-arms `recurse_cache` (ADR-0013) and records the over-share evidence.
        match self
            .audit
            .lookup(&self.recurse_cache, &arms, effective_keep_all, ast_key)
        {
            RecurseDecision::Cached(name) => Symbol::NonTerminal(NonTerminal::new(&name)),
            RecurseDecision::Mint => {
                let name = self.emit_recurse_rule(arms.clone(), effective_keep_all);
                // The audit owns the shadow-pass cache and the over-share origin map;
                // it returns whether the real pass still owns the compiled-arms entry.
                let real_pass_owns_cache =
                    !self
                        .audit
                        .record_minted(&arms, effective_keep_all, ast_key, &name);
                if real_pass_owns_cache {
                    self.recurse_cache
                        .insert((arms, effective_keep_all), name.clone());
                }
                Symbol::NonTerminal(NonTerminal::new(&name))
            }
        }
    }

    /// Emit a fresh one-or-more recurse rule for the (already-deduped) inner `arms`
    /// — Python Lark's `EBNF_to_BNF` `P: a0 | … | P a0 | …`. Returns the new
    /// helper's name; the caller records it under whichever cache key
    /// ([`recurse_helper_keyed`](Self::recurse_helper_keyed) — compiled arms in the
    /// real pass, inner-AST key in the audit shadow). Factored out so both keyings
    /// share one byte-identical emission.
    fn emit_recurse_rule(&mut self, arms: Vec<CompiledAlt>, effective_keep_all: bool) -> String {
        let name = self.fresh_anon_rule(AnonKind::Plus);
        let nt = NonTerminal::new(&name);
        let opts = RuleOptions {
            keep_all_tokens: effective_keep_all,
            ..self.anon_opts()
        };
        let k = arms.len();
        // Base arms first (orders 0..k), then the recurse arms `P a_i`
        // (orders k..2k) — Python's `EBNF_to_BNF` order, which drives the
        // `rule.order` disambiguation the resolve cases (#49/#72) rely on.
        for (i, (syms, gaps)) in arms.iter().enumerate() {
            let rule_opts = RuleOptions {
                nones_before: self.stored_output_gaps(gaps.clone()),
                ..opts.clone()
            };
            self.rules
                .push(Rule::new(nt.clone(), syms.clone(), None, rule_opts, i));
        }
        for (i, (syms, gaps)) in arms.iter().enumerate() {
            let mut rec = vec![Symbol::NonTerminal(nt.clone())];
            rec.extend_from_slice(syms);
            // The leading recurse symbol contributes no placeholder gap; shift the
            // arm's gap vector right by one to stay aligned with `rec`.
            let mut rec_gaps = vec![0];
            rec_gaps.extend_from_slice(gaps);
            let rule_opts = RuleOptions {
                nones_before: self.stored_output_gaps(rec_gaps),
                ..opts.clone()
            };
            self.rules
                .push(Rule::new(nt.clone(), rec, None, rule_opts, k + i));
        }
        name
    }

    fn compile_repeat(
        &mut self,
        inner: Expr,
        min: usize,
        max: Option<usize>,
        parent: &str,
    ) -> Result<Symbol, GrammarError> {
        match (min, max) {
            (1, None) => {
                // inner+ → one-or-more, via the shared recurse helper with the
                // inner's alternatives inlined (Python's `EBNF_to_BNF`).
                let arms = self.inner_alternatives(&inner, parent)?;
                let ast_key = inner.python_recurse_key();
                Ok(self.recurse_helper_keyed(arms, &ast_key))
            }
            (0, None) => {
                // inner* reached here only when a *single symbol* is required (the
                // common rule-position case distributes via `compile_slot`/
                // `try_distribute`, which never falls through to `compile_repeat`).
                // A `*` nested where a symbol is mandatory — e.g. inside `~n` — keeps
                // a nullable wrapper `P: <recurse> | ε` over the same shared recurse
                // helper (it cannot push its empty case up to a parent). Repeated
                // such `x*` share the wrapper, so they collapse instead of colliding
                // under LALR.
                let arms = self.inner_alternatives(&inner, parent)?;
                let plus = self.recurse_helper_keyed(arms, &inner.python_recurse_key());
                Ok(self.intern_helper(HelperKind::Star, vec![((vec![plus], vec![0, 0]), None)]))
            }
            (0, Some(1)) => {
                // inner? → optional rule. `?` adds no placeholders of its own, but
                // when nested inside a `[...]` it contributes its inner size to the
                // outer maybe's count (Lark's `FindRuleSize` takes the present arm).
                // If `inner` is *already* a nullable `?`/`*` helper, the extra `?` is
                // redundant — collapse it so `(X?)?` is just `X?`.
                let inner_sym = self.compile_expr(inner, parent)?;
                if let Symbol::NonTerminal(nt) = &inner_sym {
                    if self.nullable_opts.contains(&nt.name) {
                        return Ok(inner_sym);
                    }
                }
                Ok(
                    self.intern_helper(
                        HelperKind::Opt,
                        vec![((vec![inner_sym], vec![0, 0]), None)],
                    ),
                )
            }
            (n, Some(m)) if n == m => {
                // Exact `x~n` — Python's `_generate_repeats(rule, n, n)`. For a small
                // `n` (`n < REPEAT_BREAK_THRESHOLD`) this is one flat `__anon_rep`
                // rule of `n` copies; for a large `n` it factors into shared sub-rules
                // (`_add_repeat_rule`) so the grammar stays O(log n), not O(n).
                let inner_sym = self.compile_expr(inner, parent)?;
                Ok(self.generate_repeats(inner_sym, n, n))
            }
            (n, Some(m)) => {
                // Bounded range `x~n..m` — Python's `_generate_repeats(rule, n, m)`.
                // Small ranges stay a flat per-count `__anon_rep_range` rule; large
                // ones factor the `mn` and `diff = mx-mn` parts into shared sub-rules
                // (`_add_repeat_rule` / `_add_repeat_opt_rule`) for O(log n) size.
                let inner_sym = self.compile_expr(inner, parent)?;
                Ok(self.generate_repeats(inner_sym, n, m))
            }
            (n, None) => {
                // Unbounded `x~n..` (no max). Lark's surface grammar never produces
                // this — `~` always carries a finite max — so it is a lark-rs-only
                // edge; keep the historical `n+10`-capped flat expansion.
                let inner_sym = self.compile_expr(inner, parent)?;
                let max_count = n + 10;
                let name = self.fresh_anon_rule(AnonKind::RepRange);
                let nt = NonTerminal::new(&name);
                for count in n..=max_count {
                    let syms: Vec<Symbol> =
                        std::iter::repeat(inner_sym.clone()).take(count).collect();
                    self.rules
                        .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), count));
                }
                Ok(Symbol::NonTerminal(nt))
            }
        }
    }

    /// Lower a bounded `inner~mn..mx` into a single non-terminal that derives
    /// between `mn` and `mx` copies of `inner` — Python Lark's
    /// `EBNF_to_BNF._generate_repeats`. Below `REPEAT_BREAK_THRESHOLD` (50) the
    /// naive flat expansion (one alternative/rule per count) is fine; at or above
    /// it that lowering is O(mx²) in grammar size, so Python — and now lark-rs —
    /// **factors** the repetition into a logarithmic stack of shared sub-rules
    /// (#279 / bounty N9). Every sub-rule is a transparent `__anon_*` helper, so
    /// the produced parse tree is byte-identical to the flat expansion either way;
    /// only the build/size cost changes.
    fn generate_repeats(&mut self, rule: Symbol, mn: usize, mx: usize) -> Symbol {
        // Python's `load_grammar.REPEAT_BREAK_THRESHOLD`.
        const REPEAT_BREAK_THRESHOLD: usize = 50;
        // Python's `load_grammar.SMALL_FACTOR_THRESHOLD`.
        const SMALL_FACTOR_THRESHOLD: usize = 5;

        if mx < REPEAT_BREAK_THRESHOLD {
            // Small case: the naive per-count expansion. One `__anon_rep` /
            // `__anon_rep_range` rule, one alternative per count `n` in `mn..=mx`,
            // each `n` copies of `rule` (Python's `expansions([expansion([rule]*n)])`).
            let kind = if mn == mx {
                AnonKind::Rep
            } else {
                AnonKind::RepRange
            };
            let name = self.fresh_anon_rule(kind);
            let nt = NonTerminal::new(&name);
            for (order, count) in (mn..=mx).enumerate() {
                let syms: Vec<Symbol> = std::iter::repeat(rule.clone()).take(count).collect();
                self.rules
                    .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), order));
            }
            return Symbol::NonTerminal(nt);
        }

        // Large case: factor `rule~mn..mx` as `rule~mn rule~0..(mx-mn)`. Split `mn`
        // and `diff = mx-mn+1` with `small_factors`; build the `mn` part from
        // `_add_repeat_rule` and the `0..diff` part from `_add_repeat_opt_rule`.
        let mut mn_target = rule.clone();
        for (a, b) in Self::small_factors(mn, SMALL_FACTOR_THRESHOLD) {
            mn_target = self.add_repeat_rule(a, b, &mn_target, &rule);
        }
        if mx == mn {
            return mn_target;
        }

        // `+1` because `_add_repeat_opt_rule` matches one less than its argument.
        let diff = mx - mn + 1;
        let diff_factors = Self::small_factors(diff, SMALL_FACTOR_THRESHOLD);
        let mut diff_target = rule.clone(); // match `rule` 1 time
        let mut diff_opt_target: Vec<Symbol> = Vec::new(); // match `rule` 0 times (ε)
        let last = diff_factors.len() - 1;
        for &(a, b) in &diff_factors[..last] {
            diff_opt_target =
                vec![self.add_repeat_opt_rule(a, b, &diff_target, &diff_opt_target, &rule)];
            diff_target = self.add_repeat_rule(a, b, &diff_target, &rule);
        }
        let (a, b) = diff_factors[last];
        diff_opt_target =
            vec![self.add_repeat_opt_rule(a, b, &diff_target, &diff_opt_target, &rule)];

        // Final rule: `mn_target` followed by the `0..diff` opt part.
        let name = self.fresh_anon_rule(AnonKind::RepRange);
        let nt = NonTerminal::new(&name);
        let mut syms = vec![mn_target];
        syms.extend(diff_opt_target);
        self.rules
            .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), 0));
        Symbol::NonTerminal(nt)
    }

    /// Python Lark's `utils.small_factors`: split `n` into `[(a, b), …]` such that
    /// folding `acc = acc * a + b` (from `acc = 1`) reconstructs `n`, with each
    /// `a + b <= max_factor`. Used to factor a large bounded repeat into a
    /// logarithmic stack of sub-rules.
    fn small_factors(n: usize, max_factor: usize) -> Vec<(usize, usize)> {
        debug_assert!(max_factor > 2);
        if n <= max_factor {
            return vec![(n, 0)];
        }
        for a in (2..=max_factor).rev() {
            let (r, b) = (n / a, n % a);
            if a + b <= max_factor {
                let mut factors = Self::small_factors(r, max_factor);
                factors.push((a, b));
                return factors;
            }
        }
        unreachable!("small_factors failed to factorize {n}");
    }

    /// Python Lark's `EBNF_to_BNF._add_repeat_rule`: a transparent helper rule that
    /// matches `target` `a` times then `atom` `b` times — `__anon: target*a atom*b`.
    /// Cached on `(a, b, target, atom, opt)` (Python's `rules_cache`) so repeated
    /// chunks are shared, which is what makes the factored lowering O(log n) in
    /// size. The key omits keep-all to match Python's order-dependent shared cache
    /// verbatim — see [`repeat_cache`](GrammarCompiler::repeat_cache).
    fn add_repeat_rule(&mut self, a: usize, b: usize, target: &Symbol, atom: &Symbol) -> Symbol {
        let key = (
            a,
            b,
            target.name().to_string(),
            atom.name().to_string(),
            false,
        );
        if let Some(name) = self.repeat_cache.get(&key) {
            return Symbol::NonTerminal(NonTerminal::new(name));
        }
        let name = self.fresh_anon_rule(AnonKind::Rep);
        let nt = NonTerminal::new(&name);
        let mut syms: Vec<Symbol> = std::iter::repeat(target.clone()).take(a).collect();
        syms.extend(std::iter::repeat(atom.clone()).take(b));
        self.rules
            .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), 0));
        self.repeat_cache.insert(key, name);
        Symbol::NonTerminal(nt)
    }

    /// Python Lark's `EBNF_to_BNF._add_repeat_opt_rule`: a transparent helper that
    /// matches `atom` 0..(a*n+b)-1 times, built so it carries no shift/reduce
    /// conflict (LALR-safe). Arms:
    ///   - `target*i target_opt` for `i` in `0..a` (0 .. n*a-1 atoms), then
    ///   - `target*a atom*i`     for `i` in `0..b` (n*a .. n*a+b-1 atoms).
    /// `target_opt` is an *expansion* (a possibly-empty symbol sequence): the empty
    /// ε on the first call, a prior opt-rule non-terminal thereafter. Cached on
    /// `(a, b, target, atom, opt=true)` — Python's `rules_cache`. `target` and
    /// `target_opt` are distinct generated non-terminals whose names already encode
    /// the chain, so `(a, b, target, atom)` keys the opt-rule uniquely. Keep-all is
    /// omitted to match Python's shared cache (see
    /// [`add_repeat_rule`](Self::add_repeat_rule)).
    fn add_repeat_opt_rule(
        &mut self,
        a: usize,
        b: usize,
        target: &Symbol,
        target_opt: &[Symbol],
        atom: &Symbol,
    ) -> Symbol {
        let key = (
            a,
            b,
            target.name().to_string(),
            atom.name().to_string(),
            true,
        );
        if let Some(name) = self.repeat_cache.get(&key) {
            return Symbol::NonTerminal(NonTerminal::new(name));
        }
        let name = self.fresh_anon_rule(AnonKind::RepRange);
        let nt = NonTerminal::new(&name);
        let mut order = 0;
        for i in 0..a {
            let mut syms: Vec<Symbol> = std::iter::repeat(target.clone()).take(i).collect();
            syms.extend_from_slice(target_opt);
            self.rules
                .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), order));
            order += 1;
        }
        for i in 0..b {
            let mut syms: Vec<Symbol> = std::iter::repeat(target.clone()).take(a).collect();
            syms.extend(std::iter::repeat(atom.clone()).take(i));
            self.rules
                .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), order));
            order += 1;
        }
        self.repeat_cache.insert(key, name);
        Symbol::NonTerminal(nt)
    }
}
