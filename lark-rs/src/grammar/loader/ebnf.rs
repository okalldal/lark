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
                nones_before: Self::stored_gaps(gaps),
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
            let mut next = Vec::with_capacity(acc.len() * choices.len());
            for (psyms, pgaps) in &acc {
                for (csyms, cgaps) in &choices {
                    let mut syms = psyms.clone();
                    syms.extend_from_slice(csyms);
                    // Merge gap vectors: the seam gap is the sum of the prefix's
                    // trailing gap and the choice's leading gap.
                    let mut gaps = pgaps[..pgaps.len() - 1].to_vec();
                    gaps.push(pgaps[pgaps.len() - 1] + cgaps[0]);
                    gaps.extend_from_slice(&cgaps[1..]);
                    next.push((syms, gaps));
                }
            }
            acc = next;
        }
        // Distributing two optionals can coincide (`X? X?` → `X X | X | X | ε`);
        // identical alternatives would reduce/reduce on the same item, so keep the
        // first occurrence of each (Python's grammar dedups identical rules too).
        let mut seen = std::collections::HashSet::new();
        acc.retain(|a| seen.insert(a.clone()));
        Ok(acc)
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
                let absent_nones = if self.maybe_placeholders {
                    // A present alternative's size is its kept symbols plus any
                    // `None`s its own nested absent maybes left inline, so sizes
                    // compose through nesting exactly as Lark's `FindRuleSize`.
                    present
                        .iter()
                        .map(|(syms, gaps)| {
                            syms.iter().map(|s| self.symbol_size(s)).sum::<usize>()
                                + gaps.iter().sum::<usize>()
                        })
                        .max()
                        .unwrap_or(0)
                } else {
                    0
                };
                Ok(Some(Slot::Nullable {
                    present,
                    absent_nones,
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
            Expr::Maybe(_) if self.maybe_placeholders => Ok(None),
            Expr::Maybe(alts) => self.distributable_alternatives(alts, parent),
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
                nones_before: Self::stored_gaps(gaps.clone()),
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
    fn recurse_helper(&mut self, arms: Vec<CompiledAlt>) -> Symbol {
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
                nones_before: Self::stored_gaps(gaps.clone()),
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
                nones_before: Self::stored_gaps(rec_gaps),
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
