//! Phase 3a — terminal resolution: the terminal algebra (references,
//! alternation, repetition) → combined regexes, plus the structural
//! `PatternStr`-vs-`PatternRe` classification Python Lark keys behavior on.

use super::ast::*;
use super::compiler::GrammarCompiler;
use super::imports::common_terminals;
use crate::error::GrammarError;
use crate::grammar::terminal::{
    flags, reject_global_inline_flags, Pattern, PatternRe, PatternStr, TerminalDef,
};
use std::collections::HashMap;

impl GrammarCompiler {
    pub(super) fn get_or_create_terminal(
        &mut self,
        lit: LiteralVal,
    ) -> Result<String, GrammarError> {
        let key = format!("{:?}", lit);
        if let Some(name) = self.literal_cache.get(&key) {
            return Ok(name.clone());
        }
        // `string_type` mirrors Python's `pattern.type`: a string literal is a
        // `PatternStr` even when case-insensitive (only the flag is attached), while
        // a `/regex/` literal is a `PatternRE`. It gates the strict-mode collision
        // check (issue #35).
        let (pat, name_hint, string_type) = match &lit {
            LiteralVal::Str(s, ci) => {
                // Case-insensitive literals stay `PatternStr` (Python attaches
                // the flag without changing the pattern type), so they keep
                // string-pattern ordering and join `unless` keyword retyping.
                let pat = if *ci {
                    Pattern::Str(PatternStr::new_ci(s.as_str()))
                } else {
                    Pattern::Str(PatternStr::new(s.as_str()))
                };
                // Try to create a human-readable name from the string content
                let hint = terminal_name_hint(s);
                (pat, hint, true)
            }
            LiteralVal::Re(pattern, flags) => {
                // N3: reject a user-authored global inline flag group `(?i)`/`(?ms)`/…
                // (Python rejects it; scoped `(?flags:…)` is fine).
                reject_global_inline_flags(pattern.as_str())?;
                let pat = Pattern::Re(PatternRe::new(pattern.as_str(), *flags)?);
                (pat, None, false)
            }
        };
        let name = self.intern_anon_pattern(pat, name_hint, string_type);
        self.literal_cache.insert(key, name.clone());
        Ok(name)
    }

    /// Intern an anonymous literal/range pattern, returning the terminal name to
    /// reference it by. Unifies with an existing same-pattern terminal — named or
    /// anonymous — by adopting its name, exactly as Python Lark's
    /// `PrepareAnonTerminals` reuses the user terminal's name (so `"a"` lexes as
    /// `A` when `A: "a"` exists, and an inline `/a/` reuses `A` from `A: /a/`).
    /// Filtering is *not* keyed on this terminal — each occurrence carries its own
    /// `filter_out` — so unifying for lexing never changes a token's keep/drop fate.
    pub(super) fn intern_anon_pattern(
        &mut self,
        pat: Pattern,
        name_hint: Option<String>,
        string_type: bool,
    ) -> String {
        if let Some(existing) = self
            .terminals
            .iter()
            .find(|td| patterns_equivalent(&td.pattern, &pat))
        {
            return existing.name.clone();
        }
        // Use the clean hint when it is a fresh, valid identifier — free in both
        // the terminal and the rule namespace (`GrammarCompiler::hint_name_free`)
        // — otherwise fall back to a generated `__ANON_N` name (always a valid
        // regex group name).
        let name = match name_hint {
            Some(h) if self.hint_name_free(&h) => h,
            _ => self.fresh_terminal(),
        };
        self.terminals
            .push(TerminalDef::new(&name, pat, 0).with_string_type(string_type));
        name
    }

    /// Compile every user terminal to a regex, inlining terminal-to-terminal
    /// references (`C: "C" | D`). Resolution is order-independent and memoized;
    /// mutually-recursive terminals are rejected (a terminal denotes a *regular*
    /// language, so it cannot reference itself). Each terminal is then registered
    /// as a `Pattern::Re`, **except** one that reduces to a single case-sensitive
    /// string literal, which is registered as a `Pattern::Str` — like an inline
    /// `"literal"` and like Python Lark's `PatternStr`, so a named keyword terminal
    /// participates in the contextual lexer's `unless` keyword retyping.
    pub(super) fn resolve_terminals(&mut self) -> Result<(), GrammarError> {
        let raw_terms = std::mem::take(&mut self.raw_terms);
        let by_name: HashMap<&str, &RawTerm> =
            raw_terms.iter().map(|t| (t.name.as_str(), t)).collect();
        // Terminals already known (imports, declares) as inline-ready regex — a
        // terminal body may reference these too. This map is **mutable**: an
        // imported terminal that is `%extend`ed (the #286 case) has its entry
        // rewritten to its final, extended regex *before* any local terminal that
        // references it is resolved, so the whole set — imported terminals, their
        // pending extensions, and local `RawTerm`s — participates in one
        // dependency-resolution graph. Mutating after the resolution pass (the
        // original #286 design) could not give order-independent semantics: a local
        // terminal `X: WORD` resolved through the pre-extension `WORD` and never saw
        // a later `%extend WORD` (architect review of PR #450, defect 1).
        let mut imported: HashMap<String, String> = self
            .terminals
            .iter()
            .map(|t| (t.name.clone(), t.pattern.to_inline_regex()))
            .collect();

        // #286 / PR #450 corrective: fold every pending imported-terminal `%extend`
        // into `imported` FIRST, in dependency order, so a terminal that references
        // an extended import (or an extension whose body references another extended
        // import) resolves through the extended form. Returns the per-name final
        // regexes to write back onto the `TerminalDef`s after the raw-terminal pass.
        let pending = std::mem::take(&mut self.pending_term_extends);
        let extended_regexes = Self::resolve_pending_extends(pending, &by_name, &mut imported)?;
        // `imported` is now final. Capture the (immutable) view the later passes read.
        let imported = imported;

        let mut memo: HashMap<String, String> = HashMap::new();
        for t in &raw_terms {
            Self::resolve_term_regex(&t.name, &by_name, &imported, &mut memo, &mut Vec::new())?;
        }

        // Classify each terminal as Python would: `pattern.type == "str"` (a plain
        // string literal) vs `"re"`. lark-rs compiles everything to a regex, so we
        // recover the distinction structurally here — it gates the strict-mode
        // collision check (issue #35), which only compares the regex terminals. An
        // extended import is now a multi-arm `Pattern::Re`, so it is regex-typed
        // (`false`) regardless of its pre-extension type — clear it here so a local
        // terminal referencing it classifies correctly too.
        let imported_str: HashMap<&str, bool> = self
            .terminals
            .iter()
            .map(|t| {
                let is_str = t.string_type && !extended_regexes.contains_key(&t.name);
                (t.name.as_str(), is_str)
            })
            .collect();
        let mut str_memo: HashMap<String, bool> = HashMap::new();
        for t in &raw_terms {
            Self::term_is_str(&t.name, &by_name, &imported_str, &mut str_memo);
        }

        // The recoverable literal value (and case-insensitivity) of each
        // already-known string terminal, so a reference to an imported
        // `PatternStr` resolves to a `PatternStr` too. An extended import is no
        // longer a lone string literal, so it is excluded (it became a `Pattern::Re`).
        let imported_val: HashMap<String, (String, bool)> = self
            .terminals
            .iter()
            .filter(|t| !extended_regexes.contains_key(&t.name))
            .filter_map(|t| match &t.pattern {
                Pattern::Str(p) => Some((t.name.clone(), (p.value.clone(), p.ci))),
                _ => None,
            })
            .collect();

        // Register in source order so terminal ordering stays stable. A terminal
        // already defined via `%import` is not redefined (import wins).
        //
        // A terminal that reduces to a single string literal — case-sensitive or
        // `"..."i` — is compiled to `Pattern::Str`, exactly like an inline
        // `"literal"` and like Python Lark's `PatternStr` (which keeps the type
        // for case-insensitive literals, only attaching the flag). This is what
        // lets a named keyword terminal (`ASYNC: "async"`) join the keyword
        // `unless` retyping in the contextual lexer — otherwise it is a
        // `Pattern::Re` that ties with, and loses to, an overlapping identifier
        // regex (`NAME`), so `async` would lex as `NAME`. Everything else
        // (regex, concatenation, alternation, range, repetition) stays
        // `Pattern::Re`.
        let mut strval_memo: HashMap<String, Option<(String, bool)>> = HashMap::new();
        for t in &raw_terms {
            if self.terminals.iter().any(|td| td.name == t.name) {
                continue;
            }
            let string_type = str_memo.get(&t.name).copied().unwrap_or(false);
            let pat = match Self::term_str_value(&t.name, &by_name, &imported_val, &mut strval_memo)
            {
                Some((value, false)) => Pattern::Str(PatternStr::new(&value)),
                Some((value, true)) => Pattern::Str(PatternStr::new_ci(&value)),
                None => {
                    // Build the compiled pattern from the normalized combined regex
                    // exactly as before — `pattern`/`flags` (and so every scanner, the
                    // `unless` retype, collision, eq/hash) stay byte-identical. Then, for
                    // a terminal whose whole body is a single `/regex/` literal, override
                    // `raw` with the **verbatim** pre-normalization source so the
                    // value-length tiebreak measures `len(pattern.value)` (#399 H6-1):
                    // the normalized memo de-escapes the body (`\<\<\<` → `<<<`, `(?#…)`
                    // stripped) and would undercount the rank. A composite body keeps
                    // `raw == pattern` (the unchanged, pre-existing measure).
                    let mut re = PatternRe::new(memo[&t.name].as_str(), 0)?;
                    if let Some(src) = Self::single_re_literal(t) {
                        re.raw = src;
                    }
                    Pattern::Re(re)
                }
            };
            self.terminals
                .push(TerminalDef::new(&t.name, pat, t.priority).with_string_type(string_type));
        }

        // #286 / PR #450 corrective: the pending imported-terminal `%extend`s were
        // already resolved (in dependency order) into `imported` by
        // `resolve_pending_extends`, BEFORE the raw-terminal pass above — so a local
        // terminal that references an extended import saw the extended form. Now
        // write each final extended regex onto its `TerminalDef`. The result is
        // always a `Pattern::Re` (≥2 arms: the new alternatives plus the original
        // body), so `string_type` clears — an extended terminal is no longer a lone
        // string literal (Python's `TerminalTreeToPattern` yields a `PatternRE`).
        for (name, combined) in extended_regexes {
            if let Some(idx) = self.terminals.iter().position(|td| td.name == name) {
                self.terminals[idx].pattern = Pattern::Re(PatternRe::new(combined.as_str(), 0)?);
                self.terminals[idx].string_type = false;
            } else {
                // Unreachable: the Extend arm gated on the target being defined, and a
                // later `%override` clears the pending entry — so the terminal is
                // always present here. Guard the invariant rather than silently drop.
                debug_assert!(false, "pending extend for unknown terminal {name:?}");
            }
        }
        Ok(())
    }

    /// Resolve every pending imported-terminal `%extend` (#286) into the `imported`
    /// regex map, **in dependency order**, returning each extended terminal's final
    /// combined regex (keyed by name) for the caller to bake onto its `TerminalDef`.
    ///
    /// This is the PR #450 corrective for defect 1 (architect review): the original
    /// design applied the extensions by mutating already-baked `TerminalDef`s *after*
    /// the whole terminal-resolution pass, so a local terminal `X: WORD` (or an
    /// extension body `%extend INT: WORD`) that referenced an extended import was
    /// resolved through the import's PRE-extension regex and never saw the new
    /// alternative. Folding the extensions into `imported` here — before any
    /// referencing terminal is memoized — makes imported terminals, their pending
    /// extensions, and local raw terminals one dependency-resolution graph, giving
    /// order-independent semantics that match Python's `_extend` (which mutates the
    /// still-AST `WORD` definition before anything that references it compiles).
    ///
    /// Algorithm. The extension graph is a DAG (`extend_reaches` rejects any cycle —
    /// a terminal denotes a regular language and may not reference itself). Each
    /// terminal's final body is `sort_terminal_arms(new_alts ++ [original_body])`,
    /// where `new_alts` are resolved against the *latest* `imported`; a chain like
    /// `%extend INT: WORD` / `%extend WORD: "@"` needs `WORD` final before `INT`'s
    /// `WORD` arm is built. We reach the fixpoint by relaxation: each pass rebuilds
    /// every extended terminal's body **from its pristine original** (so arms never
    /// compound across passes) against the current `imported`, until a pass changes
    /// nothing. Monotone propagation along a DAG of depth ≤ k (the number of pending
    /// extends) converges in ≤ k passes; the change-guarded loop is bounded by
    /// `k + 1`. A *fresh* `memo` per pass is mandatory: a raw-term resolution cached
    /// against a not-yet-final `imported` would otherwise leak a stale body forward.
    /// Multiple extends of one name apply in document order, each prepending onto the
    /// running body (last-staged outermost-first), exactly as repeated `_extend`s do.
    fn resolve_pending_extends(
        pending: Vec<(String, Vec<AliasedExpansion>)>,
        by_name: &HashMap<&str, &RawTerm>,
        imported: &mut HashMap<String, String>,
    ) -> Result<HashMap<String, String>, GrammarError> {
        if pending.is_empty() {
            return Ok(HashMap::new());
        }
        // Recursion detection over the pending-extend reference graph. The main
        // `resolve_term_regex` pass catches a cycle among `RawTerm`s, but an imported
        // terminal short-circuits via the `imported` map *before* that check, so a
        // self/mutually-recursive extend body would slip through and over-accept.
        // Walk the names a pending body references, on through any `RawTerm` body
        // (`by_name`) or *other* pending-extend body, until `name` is reached
        // (recursion) or the walk dead-ends at an already-resolved imported terminal.
        let pending_refs: HashMap<&str, Vec<&AliasedExpansion>> =
            pending.iter().fold(HashMap::new(), |mut m, (n, exps)| {
                m.entry(n.as_str()).or_default().extend(exps.iter());
                m
            });
        for (name, expansions) in &pending {
            if Self::extend_reaches(name, expansions, by_name, &pending_refs, &mut Vec::new()) {
                return Err(GrammarError::Other {
                    msg: format!(
                        "Recursion in terminal {name:?} (recursion is only allowed in rules, \
                         not terminals)"
                    ),
                });
            }
        }

        // The pristine pre-extension body of every extended import — each pass
        // prepends onto THIS, never onto an already-extended body, so arms cannot
        // compound across passes.
        let original: HashMap<&str, String> = pending
            .iter()
            .filter_map(|(name, _)| {
                imported
                    .get(name.as_str())
                    .map(|body| (name.as_str(), body.clone()))
            })
            .collect();
        // Distinct extended names in first-seen (document) order, for deterministic
        // per-pass iteration.
        let mut names: Vec<&str> = Vec::new();
        for (name, _) in &pending {
            if !names.contains(&name.as_str()) {
                names.push(name.as_str());
            }
        }

        // Relax to a fixpoint: rebuild each extended body from its original against
        // the current `imported` until a pass changes nothing (≤ k + 1 passes).
        let max_passes = names.len() + 1;
        let mut final_regexes: HashMap<String, String> = HashMap::new();
        for _ in 0..max_passes {
            let mut changed = false;
            for &name in &names {
                let Some(orig_body) = original.get(name) else {
                    // Unreachable: a pending extend always targets a present import.
                    debug_assert!(false, "pending extend for unknown terminal {name:?}");
                    continue;
                };
                // Thread the body through every extend staged for this name, in
                // document order, each prepending its (freshly resolved) alternatives
                // and re-ranking — mirroring repeated `_extend` `insert(0, exp)`.
                let mut body = orig_body.clone();
                let mut memo: HashMap<String, String> = HashMap::new();
                for (entry_name, expansions) in &pending {
                    if entry_name != name {
                        continue;
                    }
                    let mut new_alts = Vec::with_capacity(expansions.len());
                    for alt in expansions {
                        let mut parts = String::new();
                        for expr in &alt.expansion {
                            parts.push_str(&Self::term_expr_regex(
                                expr,
                                by_name,
                                imported,
                                &mut memo,
                                &mut Vec::new(),
                            )?);
                        }
                        new_alts.push(parts);
                    }
                    let mut alts = new_alts;
                    alts.push(body);
                    Self::sort_terminal_arms(&mut alts)?;
                    body = alts
                        .into_iter()
                        .map(|p| format!("(?:{p})"))
                        .collect::<Vec<_>>()
                        .join("|");
                }
                if imported.get(name) != Some(&body) {
                    changed = true;
                    imported.insert(name.to_string(), body.clone());
                }
                final_regexes.insert(name.to_string(), body);
            }
            if !changed {
                break;
            }
        }
        Ok(final_regexes)
    }

    /// Sort already-resolved alternation arm regexes by Python Lark's **full**
    /// within-terminal expansion key `(-max_width, -min_width, -len(value))` —
    /// widest first, then (on a max-width tie) largest *minimum* width first, then
    /// longest source first. This mirrors `TerminalTreeToPattern` in
    /// `lark/load_grammar.py`, which sorts a terminal's flattened `expansions` by
    /// `key=(-x.max_width, -x.min_width, -len(x.value))`. The match engine is
    /// leftmost-first (`MatchKind::LeftmostFirst`), so the arm tried first must be
    /// the one Python would expand first, or a valid wider match is never taken.
    ///
    /// The `min_width` tie-break is load-bearing whenever two arms share a max
    /// width: e.g. an imported `T: /a|bc/` (max 2, **min 1**) extended with `"ab"`
    /// (max 2, **min 2**) — both width-2, but Python puts `"ab"` first on its larger
    /// min width, so `"ab"` matches as one token; ordering by source length instead
    /// (the old 2nd key) puts `a|bc` first and the engine consumes only `"a"` (#449).
    ///
    /// An unbounded arm (`max_width == None`) maps to `usize::MAX` and sorts ahead of
    /// every finite arm (Python's `MAXWIDTH`); `min_width` is always finite. Each arm
    /// is a valid regex (it was just built by the resolver / `to_inline_regex`), so
    /// re-parsing it to measure width cannot fail; the `?` is a defensive guard.
    fn sort_terminal_arms(arms: &mut [String]) -> Result<(), GrammarError> {
        // Measure each arm once (max width + min width + raw length), then sort on the
        // cached keys — avoids re-parsing the regex inside the comparator.
        let mut keyed: Vec<(usize, usize, usize, &str)> = Vec::with_capacity(arms.len());
        for arm in arms.iter() {
            let pat = Pattern::Re(PatternRe::new(arm.as_str(), 0)?);
            keyed.push((
                pat.max_width().unwrap_or(usize::MAX),
                arm_min_width(arm.as_str()),
                pat.raw_value_len(),
                arm.as_str(),
            ));
        }
        // Descending max_width, then descending min_width, then descending raw length;
        // ties keep input order (stable sort) so equal-rank arms preserve their
        // prepend sequence — Python's `(-max_width, -min_width, -len(value))`.
        keyed.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
        });
        let order: Vec<String> = keyed
            .into_iter()
            .map(|(_, _, _, s)| s.to_string())
            .collect();
        arms.clone_from_slice(&order);
        Ok(())
    }

    /// Does the `%extend` body of terminal `name` reference `name` itself —
    /// directly or transitively — making it a recursive terminal Python rejects?
    /// Walks the terminal names each expansion references, continuing through a
    /// referenced name's `RawTerm` body (`by_name`) or its own pending-extend body
    /// (`pending_refs`); an already-resolved imported terminal that is *not* a
    /// pending-extend target dead-ends the walk (its body was inlined at import, so
    /// it cannot reach the extend). `seen` guards against an unrelated `RawTerm`
    /// cycle the main pass already rejected, so this terminates.
    fn extend_reaches(
        name: &str,
        expansions: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        pending_refs: &HashMap<&str, Vec<&AliasedExpansion>>,
        seen: &mut Vec<String>,
    ) -> bool {
        let mut refs = Vec::new();
        for alt in expansions {
            for expr in &alt.expansion {
                Self::collect_term_refs(expr, &mut refs);
            }
        }
        for r in refs {
            if r == name {
                return true;
            }
            if seen.iter().any(|s| s == &r) {
                continue; // already explored this name on another path
            }
            seen.push(r.clone());
            // Follow through a same-grammar RawTerm body…
            if let Some(raw) = by_name.get(r.as_str()) {
                if Self::extend_reaches(name, &raw.expansions, by_name, pending_refs, seen) {
                    return true;
                }
            }
            // …and through another terminal's own pending-extend body (the mutual
            // imported-extend case `%extend A: B` / `%extend B: A`).
            if let Some(exps) = pending_refs.get(r.as_str()) {
                let owned: Vec<AliasedExpansion> = exps.iter().map(|e| (*e).clone()).collect();
                if Self::extend_reaches(name, &owned, by_name, pending_refs, seen) {
                    return true;
                }
            }
        }
        false
    }

    /// Collect the terminal names a terminal-body `Expr` references (recursing into
    /// repetition / group / maybe sub-expressions). Used by [`extend_reaches`] for
    /// recursion detection; rule/template references are not valid in a terminal
    /// body (the resolver rejects them) so they are ignored here.
    fn collect_term_refs(expr: &Expr, out: &mut Vec<String>) {
        match expr {
            Expr::Value(Value::Terminal(n)) => out.push(n.clone()),
            Expr::Value(_) => {}
            Expr::Repeat { inner, .. } => Self::collect_term_refs(inner, out),
            Expr::Group(alts) | Expr::Maybe(alts) => {
                for alt in alts {
                    for e in &alt.expansion {
                        Self::collect_term_refs(e, out);
                    }
                }
            }
        }
    }

    /// The string value (and case-insensitivity) iff this terminal compiles to
    /// a `PatternStr` whose value lark-rs can recover — a single string literal
    /// (case-sensitive or `"..."i`), possibly through a single-alternative group
    /// or a reference to another such terminal. Returns `None` for anything else
    /// (regex, concatenation, alternation, range, repetition). Parallels
    /// [`term_is_str`](Self::term_is_str); memoized; assumes the acyclic grammar
    /// the regex pass already validated.
    fn term_str_value(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        if let Some(v) = memo.get(name) {
            return v.clone();
        }
        if let Some(raw) = by_name.get(name) {
            memo.insert(name.to_string(), None); // cycle guard
            let result = Self::alts_str_value(&raw.expansions, by_name, imported_val, memo);
            memo.insert(name.to_string(), result.clone());
            return result;
        }
        // An imported / declared terminal: recoverable only if it is itself a
        // `PatternStr`. Common-library terminals are regex-typed → `None`.
        imported_val.get(name).cloned()
    }

    /// Value of a parenthesised/whole-terminal `expansions` node: present only when
    /// there is a single alternative that is itself a recoverable string.
    fn alts_str_value(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        if alts.len() != 1 {
            return None;
        }
        let expansion = &alts[0].expansion;
        match expansion.len() {
            0 => Some((String::new(), false)), // empty PatternStr('')
            1 => Self::expr_str_value(&expansion[0], by_name, imported_val, memo),
            _ => None, // concatenation → joined PatternRe
        }
    }

    /// Value of a single `Expr` in a terminal body (see [`term_str_value`](Self::term_str_value)).
    fn expr_str_value(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported_val: &HashMap<String, (String, bool)>,
        memo: &mut HashMap<String, Option<(String, bool)>>,
    ) -> Option<(String, bool)> {
        match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => Some((s.clone(), *ci)),
            Expr::Value(Value::Terminal(referenced)) => {
                Self::term_str_value(referenced, by_name, imported_val, memo)
            }
            Expr::Group(alts) => Self::alts_str_value(alts, by_name, imported_val, memo),
            _ => None,
        }
    }

    /// Does this terminal reduce to a single string literal (Python's `PatternStr`,
    /// `pattern.type == "str"`)? Mirrors `TerminalTreeToPattern`: an alternation, a
    /// concatenation of >1 part, a repetition, a range, or a regex literal all make
    /// it a `PatternRE`; only a lone string literal (possibly through a single-alt
    /// group or a reference to another string terminal) stays a `PatternStr`.
    /// Memoized; assumes the grammar is acyclic (the regex pass already rejected
    /// cycles).
    fn term_is_str(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if let Some(b) = memo.get(name) {
            return *b;
        }
        // A reference to an already-resolved (imported/declared) terminal, or a
        // common-library terminal (all of which are regex-typed).
        if let Some(b) = imported_str.get(name) {
            return *b;
        }
        let Some(raw) = by_name.get(name) else {
            return false; // common-library or unknown → regex-typed
        };
        // Guard against the cyclic case the regex pass would already have rejected.
        memo.insert(name.to_string(), false);
        let result = Self::alts_are_str(&raw.expansions, by_name, imported_str, memo);
        memo.insert(name.to_string(), result);
        result
    }

    /// Type of a parenthesised/whole-terminal `expansions` node: `str` only when
    /// there is a single alternative that is itself `str`.
    fn alts_are_str(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        if alts.len() != 1 {
            return false;
        }
        let expansion = &alts[0].expansion;
        match expansion.len() {
            0 => true, // empty PatternStr('')
            1 => Self::expr_is_str(&expansion[0], by_name, imported_str, memo),
            _ => false, // concatenation → joined PatternRE
        }
    }

    /// Type of a single `Expr` in a terminal body.
    fn expr_is_str(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported_str: &HashMap<&str, bool>,
        memo: &mut HashMap<String, bool>,
    ) -> bool {
        match expr {
            // A string literal is a PatternStr even when case-insensitive (Python
            // keeps the type, only attaching the flag).
            Expr::Value(Value::Literal(LiteralVal::Str(_, _))) => true,
            Expr::Value(Value::Terminal(referenced)) => {
                Self::term_is_str(referenced, by_name, imported_str, memo)
            }
            // A single-alternative group collapses to its inner pattern's type.
            Expr::Group(alts) => Self::alts_are_str(alts, by_name, imported_str, memo),
            // Regex literal, range, repetition, `?`, rule/template ref → PatternRE.
            _ => false,
        }
    }

    /// The verbatim `/…/` source iff this terminal's whole body is a **single regex
    /// literal** (`A: /…/flags`) — exactly one alternative, one expr, a `LiteralVal::Re`.
    /// This is the pre-normalization spelling Python keeps as `pattern.value` and ranks
    /// terminals by (`len(pattern.value)`, #399 H6-1); the caller overrides `PatternRe.raw`
    /// with it. The flag suffix is irrelevant to the length (Python stores flags off the
    /// value), so it is not returned. `None` for any composite body (concatenation,
    /// alternation, reference, range, repetition), which keeps today's normalized-memo
    /// measure — a pre-existing, unchanged path.
    fn single_re_literal(t: &RawTerm) -> Option<String> {
        let [alt] = t.expansions.as_slice() else {
            return None;
        };
        let [Expr::Value(Value::Literal(LiteralVal::Re(src, _flags)))] = alt.expansion.as_slice()
        else {
            return None;
        };
        Some(src.clone())
    }

    /// Resolve one terminal to its combined regex string, recursing into any
    /// referenced terminals. Memoized; `stack` carries the active resolution chain
    /// for cycle detection.
    fn resolve_term_regex(
        name: &str,
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<String, GrammarError> {
        if let Some(r) = memo.get(name) {
            return Ok(r.clone());
        }
        // Reference to an imported/declared terminal, or a common-library terminal.
        if let Some(r) = imported.get(name) {
            return Ok(r.clone());
        }
        let Some(raw) = by_name.get(name) else {
            if let Some(src) = common_terminals().get(name) {
                return Ok(src.clone());
            }
            return Err(GrammarError::UndefinedTerminal {
                name: name.to_string(),
            });
        };
        if stack.iter().any(|n| n == name) {
            stack.push(name.to_string());
            return Err(GrammarError::Other {
                msg: format!("Cyclic terminal definition: {}", stack.join(" -> ")),
            });
        }
        stack.push(name.to_string());

        // Build one regex per alternative, then join longest-first (mirroring
        // Python Lark) so a more specific alternative beats its own prefix.
        let mut alts = Vec::with_capacity(raw.expansions.len());
        for alt in &raw.expansions {
            let mut parts = String::new();
            for expr in &alt.expansion {
                parts.push_str(&Self::term_expr_regex(
                    expr, by_name, imported, memo, stack,
                )?);
            }
            alts.push(parts);
        }
        stack.pop();

        let combined = if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            alts.sort_by(|a, b| b.len().cmp(&a.len()));
            alts.into_iter()
                .map(|p| format!("(?:{p})"))
                .collect::<Vec<_>>()
                .join("|")
        };
        memo.insert(name.to_string(), combined.clone());
        Ok(combined)
    }

    /// Regex for a single `Expr` appearing in a *terminal* body. Unlike
    /// `expr_to_pattern`, a terminal reference is resolved (and inlined) rather
    /// than looked up after the fact, and flags are applied as scoped groups.
    fn term_expr_regex(
        expr: &Expr,
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<String, GrammarError> {
        let regex = match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => {
                let escaped = regex::escape(s);
                if *ci {
                    format!("(?i:{escaped})")
                } else {
                    escaped
                }
            }
            Expr::Value(Value::Literal(LiteralVal::Re(pattern, flags))) => {
                // Validate and apply any flags as a scoped group. N3: reject a global
                // inline flag group in the user's regex source first.
                reject_global_inline_flags(pattern.as_str())?;
                Pattern::Re(PatternRe::new(pattern.as_str(), *flags)?).to_inline_regex()
            }
            Expr::Value(Value::Range(from, to)) => {
                if from.chars().count() != 1 || to.chars().count() != 1 {
                    return Err(GrammarError::Other {
                        msg: "Range requires single characters".to_string(),
                    });
                }
                format!("[{}-{}]", regex::escape(from), regex::escape(to))
            }
            Expr::Value(Value::Terminal(referenced)) => {
                let inner = Self::resolve_term_regex(referenced, by_name, imported, memo, stack)?;
                format!("(?:{inner})")
            }
            Expr::Repeat {
                inner, min, max, ..
            } => {
                let inner_re = Self::term_expr_regex(inner, by_name, imported, memo, stack)?;
                let quantifier = match (*min, *max) {
                    (0, Some(1)) => "?".to_string(),
                    (1, None) => "+".to_string(),
                    (0, None) => "*".to_string(),
                    (n, Some(m)) if n == m => format!("{{{n}}}"),
                    (n, Some(m)) => format!("{{{n},{m}}}"),
                    (n, None) => format!("{{{n},}}"),
                };
                format!("(?:{inner_re}){quantifier}")
            }
            Expr::Group(alts) => {
                let parts = Self::term_alts_regex(alts, by_name, imported, memo, stack)?;
                format!("(?:{})", parts.join("|"))
            }
            Expr::Maybe(alts) => {
                let parts = Self::term_alts_regex(alts, by_name, imported, memo, stack)?;
                format!("(?:{})?", parts.join("|"))
            }
            Expr::Value(Value::Rule(name)) | Expr::Value(Value::TemplateUsage { name, .. }) => {
                return Err(GrammarError::Other {
                    msg: format!("Terminal definition cannot reference rule {name:?}"),
                });
            }
        };
        Ok(regex)
    }

    /// Regex strings for each alternative of a parenthesised group inside a
    /// terminal body (concatenating each alternative's exprs).
    fn term_alts_regex(
        alts: &[AliasedExpansion],
        by_name: &HashMap<&str, &RawTerm>,
        imported: &HashMap<String, String>,
        memo: &mut HashMap<String, String>,
        stack: &mut Vec<String>,
    ) -> Result<Vec<String>, GrammarError> {
        let mut out = Vec::with_capacity(alts.len());
        for alt in alts {
            let mut parts = String::new();
            for expr in &alt.expansion {
                parts.push_str(&Self::term_expr_regex(
                    expr, by_name, imported, memo, stack,
                )?);
            }
            out.push(parts);
        }
        Ok(out)
    }

    pub(super) fn expansion_to_pattern(&self, exprs: &[Expr]) -> Result<Pattern, GrammarError> {
        // For terminal expansions, build a regex from literals/ranges.
        let mut parts = Vec::new();
        for expr in exprs {
            let p = self.expr_to_pattern(expr)?;
            parts.push(p);
        }
        if parts.len() == 1 {
            Ok(parts.remove(0))
        } else {
            let combined = parts
                .iter()
                .map(|p| p.as_regex_str())
                .collect::<Vec<_>>()
                .join("");
            Ok(Pattern::Re(PatternRe::new(&combined, 0)?))
        }
    }

    fn expr_to_pattern(&self, expr: &Expr) -> Result<Pattern, GrammarError> {
        match expr {
            Expr::Value(Value::Literal(LiteralVal::Str(s, ci))) => {
                if *ci {
                    Ok(Pattern::Re(PatternRe::new(
                        &format!("(?i){}", regex::escape(s)),
                        flags::IGNORECASE,
                    )?))
                } else {
                    Ok(Pattern::Str(PatternStr::new(s.as_str())))
                }
            }
            Expr::Value(Value::Literal(LiteralVal::Re(p, f))) => {
                // N3: reject a user-authored global inline flag group first.
                reject_global_inline_flags(p.as_str())?;
                Ok(Pattern::Re(PatternRe::new(p.as_str(), *f)?))
            }
            Expr::Value(Value::Range(from, to)) => {
                let chars: Vec<char> = from.chars().collect();
                let chare: Vec<char> = to.chars().collect();
                if chars.len() != 1 || chare.len() != 1 {
                    return Err(GrammarError::Other {
                        msg: "Range requires single characters".to_string(),
                    });
                }
                Ok(Pattern::Re(PatternRe::new(
                    &format!("[{}-{}]", regex::escape(from), regex::escape(to)),
                    0,
                )?))
            }
            Expr::Repeat {
                inner, min, max, ..
            } => {
                let inner_pat = self.expr_to_pattern(inner)?;
                // Inside a terminal, repetition becomes a regex quantifier.
                // Bounded forms (`~n`, `~n..m`) must emit `{n}` / `{n,m}` / `{n,}`;
                // previously they fell through to "" and silently dropped the count.
                let quantifier = match (*min, *max) {
                    (0, Some(1)) => "?".to_string(),
                    (1, None) => "+".to_string(),
                    (0, None) => "*".to_string(),
                    (n, Some(m)) if n == m => format!("{{{n}}}"),
                    (n, Some(m)) => format!("{{{n},{m}}}"),
                    (n, None) => format!("{{{n},}}"),
                };
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{}){}", inner_pat.as_regex_str(), quantifier),
                    0,
                )?))
            }
            Expr::Group(alts) => {
                let parts: Vec<String> = alts
                    .iter()
                    .map(|a| {
                        let parts: Vec<Result<Pattern, GrammarError>> = a
                            .expansion
                            .iter()
                            .map(|e| self.expr_to_pattern(e))
                            .collect();
                        parts.into_iter().collect::<Result<Vec<_>, _>>().map(|ps| {
                            ps.iter()
                                .map(|p| p.as_regex_str().to_string())
                                .collect::<Vec<_>>()
                                .join("")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{})", parts.join("|")),
                    0,
                )?))
            }
            Expr::Maybe(alts) => {
                let inner_pat = self.expansion_to_pattern(&alts[0].expansion)?;
                Ok(Pattern::Re(PatternRe::new(
                    &format!("(?:{})?", inner_pat.as_regex_str()),
                    0,
                )?))
            }
            // Terminal reference in %ignore — look up the terminal's pattern
            Expr::Value(Value::Terminal(name)) => {
                if let Some(td) = self.terminals.iter().find(|t| &t.name == name) {
                    Ok(td.pattern.clone())
                } else if let Some(pat_str) = common_terminals().get(name) {
                    Ok(Pattern::Re(PatternRe::new(pat_str, 0)?))
                } else {
                    Err(GrammarError::UndefinedTerminal { name: name.clone() })
                }
            }
            _ => Err(GrammarError::Other {
                msg: format!("Cannot convert {:?} to pattern", expr),
            }),
        }
    }
}

/// The **minimum** match width of an alternation-arm regex string, in characters —
/// the lark-rs equivalent of Python's `Pattern.min_width` (`sre_parse.getwidth()[0]`),
/// the second key of `TerminalTreeToPattern`'s within-terminal arm sort
/// (`(-max_width, -min_width, -len(value))`). It is the companion of
/// [`Pattern::max_width`](crate::grammar::terminal::Pattern::max_width) and is computed
/// the *same* way per arm so the two keys can never disagree on which engine sized a
/// given arm: parse the source to a `regex-syntax` HIR and walk it; only a pattern that
/// front-end cannot parse — a lowerable-lookaround idiom (`(?=…)`, `(?<=…)`, `\b`) — is
/// sized through the shared assertion-aware [`width_range`](crate::lookaround::lower::width_range)
/// walk instead (assertions are zero-width, matching `sre_parse`). A genuine
/// backreference (which never builds a lexer) is the only residue; it falls back to `0`,
/// the conservative "smallest min" that sorts such an arm last on this key. Min width is
/// always finite (unlike max, there is no `MAXWIDTH`/∞ case).
fn arm_min_width(arm: &str) -> usize {
    match regex_syntax::parse(arm) {
        // The `regex` crate parses it (no lookaround/backref): walk the HIR.
        Ok(hir) => hir_min_width_chars(&hir),
        // The `regex` crate rejects it — size it the assertion-aware way, exactly as
        // `Pattern::max_width` falls back for the max side. `parse` failing here means a
        // real backref the analyzer also cannot size, so use `0`.
        Err(_) => crate::lookaround::parse(arm)
            .map(|node| crate::lookaround::lower::width_range(&node).0)
            .unwrap_or(0),
    }
}

/// Minimum match width of a `regex-syntax` HIR, counted in **characters**. Mirrors
/// Python's `sre_parse.getwidth()[0]`: an empty / lookaround assertion is zero-width;
/// a literal counts its code points; a class is exactly one char; a repetition's min is
/// its lower bound times the sub-pattern's min (so `a*`/`a?` contribute 0); concatenation
/// sums, and alternation takes the **minimum** over branches. Counterpart of
/// [`hir_max_width_chars`](crate::grammar::terminal) — same structure, min where that
/// takes max and the repetition lower bound where that takes the upper.
fn hir_min_width_chars(hir: &regex_syntax::hir::Hir) -> usize {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => 0,
        HirKind::Literal(lit) => std::str::from_utf8(&lit.0)
            .map(|s| s.chars().count())
            .unwrap_or(lit.0.len()),
        HirKind::Class(_) => 1,
        HirKind::Repetition(r) => hir_min_width_chars(&r.sub).saturating_mul(r.min as usize),
        HirKind::Capture(c) => hir_min_width_chars(&c.sub),
        HirKind::Concat(subs) => subs.iter().map(hir_min_width_chars).sum(),
        HirKind::Alternation(subs) => subs.iter().map(hir_min_width_chars).min().unwrap_or(0),
    }
}

/// Attempt to produce a human-readable terminal name for a literal string.
///
/// Returns `None` when the literal has no safe identifier form (e.g. it contains
/// backslashes, tabs, or other characters that are not valid in a regex named
/// capture group); the caller then assigns a fresh `__ANON_N` name. Embedding
/// raw/escaped pattern characters in the name produces invalid group names like
/// `(?P<__ANON_\>…)` and crashes regex compilation.
fn terminal_name_hint(s: &str) -> Option<String> {
    // Common punctuation uses Python Lark's names (e.g. "," -> COMMA, "(" -> LPAR).
    // Filtering is handled by `filter_out`, not a name prefix, so names are clean.
    if let Some(&name) = TERMINAL_NAMES
        .iter()
        .find(|(ch, _)| ch == &s)
        .map(|(_, n)| n)
    {
        return Some(name.to_string());
    }
    // Keyword-like strings become their uppercase form, but only when that is a
    // valid regex named-capture identifier (must not start with a digit).
    let first_ok = s
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    if first_ok && s.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Some(s.to_uppercase());
    }
    None
}

/// Two patterns are equivalent for terminal unification when they are the **same
/// kind** (both `Str` or both `Re`) and match the same language: identical regex
/// source *and* identical flags. Python Lark keys its `term_reverse` map on `Pattern`
/// equality, and `Pattern.__eq__` requires `type(self) == type(other)` — a `PatternStr`
/// never equals a `PatternRE` even when both project to the same source (`"ab"` vs
/// `/ab/`). Without the kind gate, `as_regex_str()` collapses them (both → `ab`), so a
/// literal would wrongly unify onto a same-source regex terminal and be kept instead of
/// filtered as a distinct `__ANON_*` (#403, H6-6). We also treat differing flags as
/// simply distinct, so unification never merges, say, `"a"` with `"a"i`.
fn patterns_equivalent(a: &Pattern, b: &Pattern) -> bool {
    fn flags_of(p: &Pattern) -> u32 {
        match p {
            Pattern::Str(s) if s.ci => flags::IGNORECASE,
            Pattern::Str(_) => 0,
            Pattern::Re(r) => r.flags,
        }
    }
    // Gate on matching kind (never `Str` ≡ `Re`), mirroring Python's
    // `type(self) == type(other)` in `Pattern.__eq__`.
    matches!(
        (a, b),
        (Pattern::Str(_), Pattern::Str(_)) | (Pattern::Re(_), Pattern::Re(_))
    ) && a.as_regex_str() == b.as_regex_str()
        && flags_of(a) == flags_of(b)
}

/// Standard terminal names for common punctuation/operators.
static TERMINAL_NAMES: &[(&str, &str)] = &[
    (".", "DOT"),
    (",", "COMMA"),
    (":", "COLON"),
    (";", "SEMICOLON"),
    ("+", "PLUS"),
    ("-", "MINUS"),
    ("*", "STAR"),
    ("/", "SLASH"),
    ("\\", "BACKSLASH"),
    ("|", "VBAR"),
    ("?", "QMARK"),
    ("!", "BANG"),
    ("@", "AT"),
    ("#", "HASH"),
    ("$", "DOLLAR"),
    ("%", "PERCENT"),
    ("^", "CIRCUMFLEX"),
    ("&", "AMPERSAND"),
    ("_", "UNDERSCORE"),
    ("<", "LESSTHAN"),
    (">", "MORETHAN"),
    ("=", "EQUAL"),
    ("\"", "DBLQUOTE"),
    ("'", "QUOTE"),
    ("`", "BACKQUOTE"),
    ("~", "TILDE"),
    ("(", "LPAR"),
    (")", "RPAR"),
    ("{", "LBRACE"),
    ("}", "RBRACE"),
    ("[", "LSQB"),
    ("]", "RSQB"),
    ("\n", "NEWLINE"),
    ("\r\n", "CRLF"),
    ("\t", "TAB"),
    (" ", "SPACE"),
];
