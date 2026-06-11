//! Per-terminal matching for Earley's **dynamic lexer** (Phase 2, Sprint 5).

use std::collections::HashMap;

use regex::Regex;

use super::dfa::LoweredTerminalMatcher;
use super::plan::global_flag_prefix;
use super::record_scan_skip;
use super::LexerConf;
use crate::error::GrammarError;
use crate::grammar::intern::SymbolId;

/// A matcher for Earley's **dynamic lexer** (Phase 2, Sprint 5).
///
/// Unlike the [`Scanner`](super::scanner::Scanner), which scans one combined
/// alternation left-to-right and hands the parser a fixed token stream, the dynamic
/// lexer matches a *specific* terminal — the one an Earley item predicts — at a
/// given position, integrating scanning into the parse loop. Each terminal
/// therefore gets its own compiled regex, anchored at the query position via
/// [`Regex::find_at`] (a match is accepted only if it begins exactly at `pos`).
///
/// There is **no `unless` keyword retyping** here: the parser context (which items
/// sit in the scan set) already decides which terminals to try, so `if`-vs-`iffy`
/// is resolved by the grammar, not by a lexer tie-break. Per-terminal flags
/// (`(?i:…)`) and `g_regex_flags` are preserved exactly as the basic lexer does.
pub struct DynamicMatcher {
    res: HashMap<SymbolId, TermRegex>,
    ignore: Vec<SymbolId>,
    names: HashMap<SymbolId, String>,
}

/// One terminal's per-terminal matcher for the dynamic lexer: the `regex` crate for
/// the plain common case, the lowered single-terminal DFA for a lookaround terminal.
enum TermRegex {
    Plain(Regex),
    Lowered(LoweredTerminalMatcher),
}

impl TermRegex {
    /// End of the non-empty match starting exactly at `pos`, or `None` — the
    /// contract `AnyRegex::match_end_at` had. The full `text` (not a suffix) is
    /// passed so a lookbehind can see the bytes before `pos`, exactly as the
    /// historical fancy probe could.
    fn match_end_at(&self, text: &str, pos: usize) -> Option<usize> {
        match self {
            TermRegex::Plain(re) => {
                let m = re.find_at(text, pos);
                record_scan_skip(pos, m.as_ref().map(|m| m.start()));
                let m = m?;
                (m.start() == pos && m.end() > pos).then_some(m.end())
            }
            TermRegex::Lowered(m) => m.match_end_at(text, pos),
        }
    }

    /// End offset of a non-empty match anchored at the start of `sub`, or `None` —
    /// the contract `AnyRegex::match_end_in` had (used by the `dynamic_complete`
    /// scan, which re-matches against a truncated haystack).
    fn match_end_in(&self, sub: &str) -> Option<usize> {
        match self {
            TermRegex::Plain(re) => {
                let m = re.find(sub)?;
                (m.start() == 0 && m.end() > 0).then_some(m.end())
            }
            TermRegex::Lowered(m) => m.match_end_in(sub),
        }
    }
}

impl DynamicMatcher {
    /// Build a matcher from the same [`LexerConf`] the basic lexer uses, so both
    /// engines honour identical terminal patterns and global flags. A lookaround
    /// terminal lowers to its own single-terminal DFA ([`LoweredTerminalMatcher`]);
    /// a terminal the lowering refuses fails the build with the **same categorized
    /// scope error** the basic-lexer path produces (`docs/LOOKAROUND_SCOPE.md`), so
    /// the dynamic lexer accepts exactly the same grammars.
    pub fn new(conf: &LexerConf) -> Result<Self, GrammarError> {
        let prefix = global_flag_prefix(conf.global_flags);
        let mut res = HashMap::new();
        for (id, term) in &conf.terminals {
            let src = format!("{}{}", prefix, term.pattern.to_inline_regex());
            let compiled = match Regex::new(&src) {
                Ok(re) => TermRegex::Plain(re),
                Err(e) => TermRegex::Lowered(LoweredTerminalMatcher::build(
                    *id,
                    term,
                    conf.global_flags,
                    &e.to_string(),
                )?),
            };
            res.insert(*id, compiled);
        }
        Ok(DynamicMatcher {
            res,
            ignore: conf.ignore.clone(),
            names: conf.names(),
        })
    }

    /// Match terminal `id` starting exactly at byte `pos` in `text`. Returns the
    /// matched slice, or `None` if the terminal does not match there (or matches
    /// empty — a nullable terminal can never advance the scan).
    pub fn match_at<'t>(&self, id: SymbolId, text: &'t str, pos: usize) -> Option<&'t str> {
        let end = self.res.get(&id)?.match_end_at(text, pos)?;
        Some(&text[pos..end])
    }

    /// Match terminal `id` against the whole sub-slice `sub` (anchored at its
    /// start). Used by `dynamic_complete` to explore shorter tokenizations, which
    /// Python Lark does by re-matching against a truncated string `s[:-j]`.
    pub fn match_in<'t>(&self, id: SymbolId, sub: &'t str) -> Option<&'t str> {
        let end = self.res.get(&id)?.match_end_in(sub)?;
        Some(&sub[..end])
    }

    /// The `%ignore` terminal ids, tried between tokens by the dynamic scanner.
    pub fn ignore(&self) -> &[SymbolId] {
        &self.ignore
    }

    /// Display name of a terminal id (for the token's `type_`).
    pub fn name(&self, id: SymbolId) -> &str {
        self.names.get(&id).map(String::as_str).unwrap_or("")
    }
}
