//! Pattern-string flag algebra: the whole-pattern flag wrapper the grammar
//! loader bakes (`(?is:…)`) and its inverse, plus the verbose-mode probes the
//! routing seam guards with. Pure string/AST manipulation — no engine here.

/// Wrap `src` in a flag-scoped group `(?flags:src)` for a terminal's own regex flags,
/// or return it unchanged when the terminal has none. Mirrors
/// [`Pattern::to_inline_regex`](crate::grammar::terminal::Pattern::to_inline_regex)
/// so a lowered branch's flags scope exactly as the un-split terminal's did.
pub(super) fn wrap_flags(flags: u32, src: &str) -> String {
    let letters = crate::grammar::terminal::flag_letters(flags);
    if letters.is_empty() {
        src.to_string()
    } else {
        format!("(?{letters}:{src})")
    }
}

/// Strip a **whole-pattern flag wrapper** `(?ims:…)` back into the flag bitset —
/// the inverse of what the grammar loader bakes in. The loader converts a terminal's
/// `/…/is`-style flags into one flag-scoped group around the entire pattern
/// (`Pattern::to_inline_regex`) and stores `PatternRe.flags = 0`, so without this
/// step the lowering router would see every assertion nested inside a `Group` (an
/// instant decline/reject) and the bundled `python.STRING` / `python.LONG_STRING`
/// idioms would silently ride the fancy fallback — with their flags lost if a
/// recognizer peeled the group instead (the dotall mis-lowering the
/// `g_regex_flags_dotall_long_string` / `newline_dotall_body` seam fixtures pin).
///
/// Returns the inner pattern + the merged flags. Conservative: on anything but a
/// single unquantified positive-`ims` flag group spanning the whole pattern (an `x`
/// VERBOSE wrapper — see the inline note, a `-` clear, an unknown letter, a
/// quantifier, a bare `(?:`, a parse failure) the input is returned unchanged, so
/// the route behaves exactly as before. Loops so a nested `(?i:(?s:…))` (not
/// produced by the loader, but cheap to honor) fully unwraps.
pub(super) fn strip_whole_pattern_flag_wrapper(raw: &str, flags: u32) -> (String, u32) {
    use crate::grammar::terminal::flags as f;
    let mut pattern = raw.to_string();
    let mut flags = flags;
    loop {
        let Ok(crate::lookaround::Node::Group { open, body, quant }) =
            crate::lookaround::parse(&pattern)
        else {
            return (pattern, flags);
        };
        if !quant.is_empty() {
            return (pattern, flags);
        }
        let Some(letters) = open
            .strip_prefix("(?")
            .and_then(|s| s.strip_suffix(':'))
            .filter(|s| !s.is_empty())
        else {
            return (pattern, flags); // a capturing `(` or bare `(?:` — not a flag wrapper
        };
        let mut add = 0u32;
        for c in letters.chars() {
            add |= match c {
                'i' => f::IGNORECASE,
                'm' => f::MULTILINE,
                's' => f::DOTALL,
                // `x` (VERBOSE) is deliberately NOT stripped: the lookaround
                // parser and its width/offset analysis are not verbose-aware, so a
                // stripped `(?x:…)` body would have its whitespace/comments counted
                // as literal width while the re-wrapped branch ignores them — a
                // fixed-offset lookbehind could lower with a wrong offset (a
                // false-accept). Left wrapped, the pattern is refused with the
                // honest categorized NYI error (`DeclineReason::VerboseMode`,
                // via `is_verbose_wrapped_lookaround`) — the reject-when-unsure
                // direction. Pinned by
                // `verbose_flag_wrapper_is_not_stripped_into_lowering`.
                //
                // A flag-clear (`-`) or any unknown letter likewise leaves the
                // pattern alone. Named groups (`(?P<n>…`, `(?<n>…`) never get here —
                // their opens end with `>`, not `:`.
                _ => return (pattern, flags),
            };
        }
        flags |= add;
        pattern = body.to_source();
    }
}

/// True when `raw` is a whole-pattern flag wrapper whose letters include `x` (VERBOSE)
/// **and** whose body contains a lookaround assertion — the shape
/// [`strip_whole_pattern_flag_wrapper`] deliberately refuses to strip (the lookaround
/// analyzer's width/offset arithmetic is not verbose-aware). Detected *before*
/// classification so the refusal surfaces as the honest
/// [`DeclineReason::VerboseMode`](crate::lookaround::classify::DeclineReason::VerboseMode)
/// (NotYetImplemented) instead of the classifier
/// mislabeling the group-nested assertion as out-of-scope internal lookahead. An
/// `x`-wrapped pattern with **no** assertion never reaches the routing seam (the
/// `regex` crate supports verbose mode and compiles it plain), and one that is
/// regex-rejected for a non-lookaround reason (e.g. a backref) falls through to the
/// `BacktrackingOnlySyntax` triage.
pub(super) fn is_verbose_wrapped_lookaround(raw: &str) -> bool {
    let Ok(crate::lookaround::Node::Group { open, body, quant }) = crate::lookaround::parse(raw)
    else {
        return false;
    };
    let Some(letters) = open.strip_prefix("(?").and_then(|s| s.strip_suffix(':')) else {
        return false;
    };
    quant.is_empty()
        && !letters.is_empty()
        && letters.chars().all(|c| matches!(c, 'i' | 'm' | 's' | 'x'))
        && letters.contains('x')
        && body.has_assertion()
}

/// True when the lookaround frontend parses `raw` and finds any assertion in it.
/// Used by the routing seam's verbose-mode gate: under VERBOSE the analyzer's
/// width/offset arithmetic would be wrong, and the only route that could *lower*
/// such a pattern is one with assertions — so refusing exactly these closes the
/// false-accept class. A pattern the frontend cannot parse returns `false` and is
/// still refused downstream (`DeclineReason::FrontendParse` / `LoweringRoute::Plain`'s
/// `BacktrackingOnlySyntax` triage), never lowered.
pub(super) fn pattern_contains_assertion(raw: &str) -> bool {
    crate::lookaround::parse(raw).is_ok_and(|n| n.has_assertion())
}
