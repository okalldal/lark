//! Phase 3a — terminal resolution: the terminal algebra (references,
//! alternation, repetition) → combined regexes, plus the structural
//! `PatternStr`-vs-`PatternRe` classification Python Lark keys behavior on.

use super::ast::*;
use super::compiler::GrammarCompiler;
use super::imports::common_terminals;
use crate::error::GrammarError;
use crate::grammar::terminal::{flags, Pattern, PatternRe, PatternStr, TerminalDef};
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
        // Use the clean hint when it is a fresh, valid identifier; otherwise fall
        // back to a generated `__ANON_N` name (always a valid regex group name).
        let name = match name_hint {
            Some(h) if !self.terminals.iter().any(|t| t.name == h) => h,
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
        // terminal body may reference these too.
        let imported: HashMap<String, String> = self
            .terminals
            .iter()
            .map(|t| (t.name.clone(), t.pattern.to_inline_regex()))
            .collect();

        let mut memo: HashMap<String, String> = HashMap::new();
        for t in &raw_terms {
            Self::resolve_term_regex(&t.name, &by_name, &imported, &mut memo, &mut Vec::new())?;
        }

        // Classify each terminal as Python would: `pattern.type == "str"` (a plain
        // string literal) vs `"re"`. lark-rs compiles everything to a regex, so we
        // recover the distinction structurally here — it gates the strict-mode
        // collision check (issue #35), which only compares the regex terminals.
        let imported_str: HashMap<&str, bool> = self
            .terminals
            .iter()
            .map(|t| (t.name.as_str(), t.string_type))
            .collect();
        let mut str_memo: HashMap<String, bool> = HashMap::new();
        for t in &raw_terms {
            Self::term_is_str(&t.name, &by_name, &imported_str, &mut str_memo);
        }

        // The recoverable literal value (and case-insensitivity) of each
        // already-known string terminal, so a reference to an imported
        // `PatternStr` resolves to a `PatternStr` too.
        let imported_val: HashMap<String, (String, bool)> = self
            .terminals
            .iter()
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
                None => Pattern::Re(PatternRe::new(memo[&t.name].as_str(), 0)?),
            };
            self.terminals
                .push(TerminalDef::new(&t.name, pat, t.priority).with_string_type(string_type));
        }
        Ok(())
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
                // Validate and apply any flags as a scoped group.
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
            Expr::Repeat { inner, min, max } => {
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
            Expr::Repeat { inner, min, max } => {
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

/// Two patterns are equivalent for terminal unification when they match the same
/// language: identical regex source *and* identical flags. Python Lark keys its
/// `term_reverse` map on `Pattern` equality (and raises on a flag mismatch for the
/// same source); we treat differing flags as simply distinct, so unification never
/// merges, say, `"a"` with `"a"i`.
fn patterns_equivalent(a: &Pattern, b: &Pattern) -> bool {
    fn flags_of(p: &Pattern) -> u32 {
        match p {
            Pattern::Str(s) if s.ci => flags::IGNORECASE,
            Pattern::Str(_) => 0,
            Pattern::Re(r) => r.flags,
        }
    }
    a.as_regex_str() == b.as_regex_str() && flags_of(a) == flags_of(b)
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
    ("\t", "TAB"),
    (" ", "SPACE"),
];
