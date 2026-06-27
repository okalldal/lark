//! Phase 4a — parameterized template instantiation (`_sep{x, sep}: x (sep x)*`).

use super::ast::*;
use super::compiler::GrammarCompiler;
use crate::error::GrammarError;
use crate::grammar::rule::{Rule, RuleOptions};
use crate::grammar::symbol::{NonTerminal, Symbol};
use std::collections::HashMap;

impl GrammarCompiler {
    pub(super) fn instantiate_template(
        &mut self,
        name: &str,
        args: Vec<Value>,
        _parent: &str,
    ) -> Result<Symbol, GrammarError> {
        let (params, expansions, modifiers, priority) = self
            .templates
            .get(name)
            .ok_or_else(|| GrammarError::UndefinedRule {
                name: name.to_string(),
            })?
            .clone();

        if params.len() != args.len() {
            return Err(GrammarError::Other {
                msg: format!(
                    "Template {} expects {} args, got {}",
                    name,
                    params.len(),
                    args.len()
                ),
            });
        }

        // Memoize by (name, args): a repeat request for the same instantiation —
        // including the self-reference inside a recursive template — resolves to the
        // rule already being built rather than instantiating (and recursing) again.
        let key = format!("{}::{:?}", name, args);
        if let Some(existing) = self.template_instances.get(&key) {
            return Ok(Symbol::NonTerminal(NonTerminal::new(existing)));
        }

        // Name the instance `base{N}`: the `{` marks it as a template instance whose
        // *tree label* is the base name (Lark's `template_source`), and leaving the
        // base prefix intact means a `_`-prefixed template (`_expr`) instantiates to
        // a transparent rule while `expr` does not. The counter keeps distinct
        // arg-sets distinct. Registered *before* compiling the body so a
        // self-reference resolves to the rule being built.
        let inst_name = format!("{}{{{}}}", name, self.anon_counter);
        self.anon_counter += 1;
        self.template_instances.insert(key, inst_name.clone());

        // Build substitution map
        let subst: HashMap<String, Value> = params.into_iter().zip(args).collect();

        // Each instance inherits the template's own rule options (keep-all / expand1
        // / priority), not the anon-helper defaults — so `!expr{t}` keeps its tokens.
        let keep_all = modifiers.contains('!') || self.global_keep_all;
        let inst_opts = RuleOptions {
            expand1: modifiers.contains('?'),
            keep_all_tokens: keep_all,
            priority,
            ..RuleOptions::default()
        };
        // Make keep-all visible to placeholder counting while this body compiles,
        // then restore the caller's context.
        let saved_keep_all = self.current_keep_all;
        self.current_keep_all = keep_all;

        // Inlined-rule placement validation against the template's *base* name (the
        // tree label, e.g. `_x`), exactly as Python Lark checks `?_x{a}` and an
        // alias on `_x{a}: a -> al` (RC4a/RC4b) — `inst_name` is `base{N}` so the
        // base name carries the `_` prefix.
        Self::validate_inlined_rule_placement(name, inst_opts.expand1, &expansions)?;

        // Substitute template params in expansions
        let expansions = Self::substitute_template(&expansions, &subst);
        let origin = NonTerminal::new(&inst_name);
        // RC4c: a group-internal alias is rejected (a rule reference, not a tree
        // label) — `reject_nested: true`, exactly as for a named rule body.
        let compiled = self.compile_alternatives(expansions, &inst_name, true)?;
        for (order, ((syms, gaps), alias)) in compiled.into_iter().enumerate() {
            let options = RuleOptions {
                nones_before: self.stored_output_gaps(gaps),
                ..inst_opts.clone()
            };
            self.rules
                .push(Rule::new(origin.clone(), syms, alias, options, order));
        }
        self.current_keep_all = saved_keep_all;
        Ok(Symbol::NonTerminal(origin))
    }

    fn substitute_template(
        expansions: &[AliasedExpansion],
        subst: &HashMap<String, Value>,
    ) -> Vec<AliasedExpansion> {
        expansions
            .iter()
            .map(|alt| AliasedExpansion {
                expansion: alt
                    .expansion
                    .iter()
                    .map(|e| Self::subst_expr(e, subst))
                    .collect(),
                alias: alt.alias.clone(),
            })
            .collect()
    }

    fn subst_expr(expr: &Expr, subst: &HashMap<String, Value>) -> Expr {
        match expr {
            Expr::Value(v) => Expr::Value(Self::subst_value(v, subst)),
            Expr::Repeat {
                inner,
                min,
                max,
                kind,
            } => Expr::Repeat {
                inner: Box::new(Self::subst_expr(inner, subst)),
                min: *min,
                max: *max,
                kind: *kind,
            },
            Expr::Group(alts) => Expr::Group(Self::substitute_template(alts, subst)),
            Expr::Maybe(alts) => Expr::Maybe(Self::substitute_template(alts, subst)),
        }
    }

    /// Substitute template params inside a `Value`. Crucially this recurses into a
    /// nested template usage's arguments, so `_sep{item, delim}` inside a `_sep`
    /// body becomes `_sep{NUMBER, ","}` — the self-instantiation the memo then
    /// collapses, rather than a reference to undefined `item`/`delim` rules.
    fn subst_value(v: &Value, subst: &HashMap<String, Value>) -> Value {
        match v {
            Value::Rule(name) | Value::Terminal(name) => {
                subst.get(name).cloned().unwrap_or_else(|| v.clone())
            }
            // Higher-order templates: a parameter can itself be a template applied
            // as `t{…}`. Substitute the *usage's name* too (`t` → `b`), so
            // `a{t}: t{"a"}` with `a{b}` instantiates `b{"a"}`, not undefined `t`.
            Value::TemplateUsage { name, args } => {
                let name = match subst.get(name) {
                    Some(Value::Rule(n)) | Some(Value::Terminal(n)) => n.clone(),
                    _ => name.clone(),
                };
                Value::TemplateUsage {
                    name,
                    args: args.iter().map(|a| Self::subst_value(a, subst)).collect(),
                }
            }
            other => other.clone(),
        }
    }
}
