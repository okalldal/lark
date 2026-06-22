//! Phase 4 — rule-body compilation: EBNF expansion, leading-nullable
//! distribution, and anonymous-helper sharing (AST `Expr`s → flat BNF [`Rule`]s).

use super::ast::*;
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
    /// `None` placeholders (nonzero only for a `maybe_placeholders` `[...]`,
    /// mirroring Python Lark's `_EMPTY` markers → `empty_indices`).
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
        let origin = NonTerminal::new(&raw.name);
        // Make keep_all visible to placeholder counting while this rule's body
        // (and the anonymous rules it expands into) is compiled.
        self.current_keep_all = keep_all;

        // Each source alternative may distribute into several BNF alternatives
        // (a leading nullable fanned out), so `order` runs over the flattened
        // result rather than the raw alternatives — after the cross-alternative
        // dedup + collision check (Python numbers post-dedup too).
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::new();
        for alt in raw.expansions.into_iter() {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, &origin.name, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        let compiled = Self::dedup_and_check_alts(&origin.name, compiled)?;
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
            acc = Self::concat_alts(&acc, &choices);
        }
        // Distributing two optionals can coincide (`X? X?` → `X X | X | X | ε`);
        // identical alternatives would reduce/reduce on the same item, so keep the
        // first occurrence of each (Python's grammar dedups identical rules too).
        //
        // Under `maybe_placeholders=False` an *empty* alternative carries no output
        // role — its maybe `_EMPTY` markers are stripped before tree build — so two
        // empty arms that differ only in those markers are duplicate *empty* rules,
        // which Python tolerates and dedups (`load_grammar.py`: the "Rules defined
        // twice" check fires only for non-empty `dups[0].expansion`). Collapsing them
        // here keeps a non-colliding nullable like `([A])?` from minting two empty
        // productions at one origin — an LALR conflict Python never reports. A
        // *non-empty* arm keeps its markers so a genuine `[A]~2 C`-style collision
        // still reaches `dedup_and_check_alts`.
        let canon = |a: &CompiledAlt| -> CompiledAlt {
            if !self.maybe_placeholders && a.0.is_empty() {
                (Vec::new(), vec![0])
            } else {
                a.clone()
            }
        };
        let mut seen = std::collections::HashSet::new();
        acc.retain(|a| seen.insert(canon(a)));
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
        // A bounded `~n` / `~n..m` (the `?`/`*`/`+` shapes were consumed by
        // `try_distribute` / the group check above; only `max: Some(_)` exact/range
        // counts reach here) inlines into the parent's alternatives rather than
        // minting a helper rule, matching Python's `_generate_repeats` (#176).
        if let Expr::Repeat {
            inner,
            min,
            max: Some(max),
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
            // `X?` / `(...)?` → present forms of the inner.
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
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
            } => {
                let arms = self.inner_alternatives(inner, parent)?;
                let plus = self.recurse_helper(arms);
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
    /// leading nullable. Returns `None` when the expr cannot be safely distributed
    /// — a `maybe_placeholders` `[X]` *nested under another nullable wrapper*
    /// (e.g. `([X])?`), whose absent-with-placeholders middle alternative this
    /// present/absent split cannot represent — so the caller keeps the helper.
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
            // `[X]` without placeholders is a plain optional group; with
            // placeholders this nested position cannot carry the absent case's
            // `None`s (see the doc comment), so keep the helper.
            //
            // Under `maybe_placeholders` the absent-with-`None`s middle alternative
            // of a nested `[X]` cannot ride this present/absent split, so keep the
            // helper. Without placeholders, the maybe's own absent arm is included
            // as a present form carrying its positional `_EMPTY` markers (the same
            // `[_EMPTY] * FindRuleSize` Python's `maybe` always emits), so a colliding
            // `[A]~0..1 C` / `([A])? C` — where the absent-`[X]` and the outer ε both
            // reduce to `start -> C` — reaches `dedup_and_check_alts` and is rejected
            // in both modes (#252). The redundant *empty* arm (when there is no tail
            // to attach the marker to, e.g. a lone `([A])?`) collapses against the
            // outer ε in `compile_expansion`'s `maybe_placeholders=False`
            // empty-arm canonicalization, so a non-colliding nullable still builds.
            Expr::Maybe(_) if self.maybe_placeholders => Ok(None),
            Expr::Maybe(alts) => {
                let mut present = match self.distributable_alternatives(alts, parent)? {
                    Some(p) => p,
                    None => return Ok(None),
                };
                let absent_nones = self.maybe_absent_size(&present);
                present.push((Vec::new(), vec![absent_nones]));
                Ok(Some(present))
            }
            // A nested `?` collapses: `(X?)?` ≡ `X?`, so drop the inner optionality
            // and let the outer distribution re-add the single ε.
            Expr::Repeat {
                inner,
                min: 0,
                max: Some(1),
            } => self.present_forms(*inner, parent),
            // `X*` / `X+` present form is the shared one-or-more recurse helper
            // (inner arms inlined, Python's `EBNF_to_BNF`).
            Expr::Repeat {
                inner,
                min: 0,
                max: None,
            }
            | Expr::Repeat {
                inner,
                min: 1,
                max: None,
            } => {
                let arms = self.inner_alternatives(&inner, parent)?;
                let plus = self.recurse_helper(arms);
                Ok(single(plus))
            }
            // Bounded `~n` / `~n..m` nested under a distributed `?` inlines its
            // counts as present forms too (the directly-positioned case is handled
            // in `compile_slot`); large/aliased repeats fall back to the helper.
            Expr::Repeat {
                inner,
                min,
                max: Some(max),
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
            Expr::Repeat { inner, min, max } => self.compile_repeat(*inner, min, max, parent),
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
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        for alt in alts {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, parent, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        // Same dedup + collision check as a named rule's alternatives: Python
        // inlines groups into the parent, where its `expansions` dedup and
        // "Rules defined twice" check run — so `(X | X)` collapses (and then
        // takes the single-symbol shortcut below, like Python's inlined `X`),
        // and `([A] [A] B)` is rejected at load.
        let compiled = Self::dedup_and_check_alts(parent, compiled)?;
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
        match kind {
            // `(...)` is spliced inline with no empty arm.
            HelperKind::Group => {}
            // A placeholder-less optional group: just an empty alternative.
            HelperKind::GroupOptional => {
                self.rules.push(Rule::new(
                    origin.clone(),
                    vec![],
                    None,
                    self.anon_opts(),
                    100,
                ));
            }
            // `[...]` under maybe_placeholders: the empty case emits one `None`
            // per kept slot of the widest alternative.
            HelperKind::Maybe => {
                let empty_opts = RuleOptions {
                    placeholder_count: max_size,
                    ..self.anon_opts()
                };
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, empty_opts, 100));
            }
            // `x?` / `x*`: a single-arm nullable wrapper `P: inner | ε`.
            HelperKind::Opt | HelperKind::Star => {
                self.nullable_opts.insert(name.clone());
                self.rules
                    .push(Rule::new(origin.clone(), vec![], None, self.anon_opts(), 1));
            }
        }
        if cacheable {
            self.helper_cache.insert(key, name);
        }
        Symbol::NonTerminal(origin)
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
        let mut compiled: Vec<(CompiledAlt, Option<String>)> = Vec::with_capacity(alts.len());
        for alt in alts {
            let alias = alt.alias.clone();
            for alt_c in self.compile_expansion(alt.expansion, parent, true)? {
                compiled.push((alt_c, alias.clone()));
            }
        }
        // Same dedup + collision check as a named rule's alternatives (Python
        // distributes `[...]` into the parent, where they run; see
        // `dedup_and_check_alts`).
        let compiled = Self::dedup_and_check_alts(parent, compiled)?;
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
    fn recurse_helper(&mut self, mut arms: Vec<CompiledAlt>) -> Symbol {
        // Dedup identical arms (first occurrence wins, order preserved). Python
        // Lark's `EBNF_to_BNF` builds the one-or-more rule from the *set* of inner
        // expansions, so `("b" | "b")*` collapses to a single recurse arm. Without
        // this, two byte-identical arms emit two identical base reductions (and two
        // identical `P arm` recurse reductions) into the same state — an
        // unresolvable reduce/reduce Python never reports (#210, seed 99). Order is
        // preserved because `rule.order` drives the resolve disambiguation (#49/#72).
        {
            let mut seen: std::collections::HashSet<CompiledAlt> = std::collections::HashSet::new();
            arms.retain(|arm| seen.insert(arm.clone()));
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
        let key = (arms.clone(), effective_keep_all);
        if let Some(name) = self.recurse_cache.get(&key) {
            return Symbol::NonTerminal(NonTerminal::new(name));
        }
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
        self.recurse_cache.insert(key, name);
        Symbol::NonTerminal(nt)
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
                Ok(self.recurse_helper(arms))
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
                let plus = self.recurse_helper(arms);
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
                // exact repetition: inline n copies
                let inner_sym = self.compile_expr(inner, parent)?;
                let name = self.fresh_anon_rule(AnonKind::Rep);
                let nt = NonTerminal::new(&name);
                let syms: Vec<Symbol> = std::iter::repeat(inner_sym).take(n).collect();
                self.rules
                    .push(Rule::new(nt.clone(), syms, None, self.anon_opts(), 0));
                Ok(Symbol::NonTerminal(nt))
            }
            (n, max_opt) => {
                // Range: generate rules for n..m repetitions
                let inner_sym = self.compile_expr(inner, parent)?;
                let max_count = max_opt.unwrap_or(n + 10); // cap at n+10 for unbounded
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
}
