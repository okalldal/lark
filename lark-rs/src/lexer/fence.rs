//! The fence-idiom matcher (idiom #5, `docs/LOOKAROUND_SCOPE.md`): tag-echo
//! delimited tokens — heredocs (`<<TAG … TAG`) and CMake bracket arguments
//! (`[==[ … ]==]`). The pattern shape is recognized by
//! [`recognize_fence_idiom`](crate::lookaround::lower::recognize_fence_idiom)
//! (a raw string parser — the lookaround AST parser cannot read named
//! backreferences) and matched here by a two-phase scanner that needs no
//! backtracking engine.
//!
//! These patterns are **non-regular** (the tag is echoed verbatim at the
//! close), so they bypass the lookaround refusal seam entirely: the `regex`
//! crate's rejection is correct — they cannot lower into a DFA — but they are
//! linear-time recognisable per attempt.
//!
//! **Cost accounting.** One match attempt is O(remaining input): phase 2 scans
//! forward for the close sequence (and on failure scans to end-of-input).
//! That is the same worst case Python `re` pays for the identical lazy-body
//! pattern (`(?:.|\n)*?TAG` walks forward one char per lazy step), so oracle
//! parity extends to the complexity class. The scan is charged to the
//! [`lexer_scan_steps`](crate::perf) counter so the deterministic
//! lexer-scaling gate sees fence work like any other forward scan.

use regex_automata::{
    dfa::{dense, Automaton},
    Anchored, Input, MatchKind,
};

use super::dfa::build_combined_dfa;
use crate::error::GrammarError;
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, TerminalDef};
use crate::lookaround::lower::FenceSpec;

/// A compiled fence-idiom terminal: `OPEN(?P<NAME>TAG_RE)SEP BODY (?P=NAME)CLOSE_POST`.
///
/// Matched in two phases:
/// 1. Check the `open` literal, run `tag_dfa` anchored after it, check `sep`.
/// 2. Build `close_seq = close_pre ++ tag_bytes ++ close_post`; scan the
///    remaining input for its first occurrence at least `body_min` chars in.
///
/// The recognizer guarantees the body unit is universal (`[\s\S]` / `.|\n`)
/// under a lazy quantifier, so "first close occurrence ≥ `body_min` chars in"
/// is exactly Python's lazy-body semantics; see
/// [`FenceSpec`] for the one residual tag/separator assumption.
pub(super) struct FenceMatcher {
    /// The terminal's interned id — returned as the match type on success.
    pub(super) id: SymbolId,
    /// The terminal's rank in the plan, for competing with plain terminals.
    pub(super) rank: usize,
    /// Literal bytes that must appear at the start of the match.
    open: Vec<u8>,
    /// Anchored leftmost-first DFA for the tag pattern (greedy, no backtracking
    /// into the separator — see `FenceSpec`).
    tag_dfa: dense::DFA<Vec<u32>>,
    /// Literal bytes between tag end and the body.
    sep: Vec<u8>,
    /// Minimum number of body characters (0 for `*?`, 1 for `+?`).
    body_min: usize,
    /// Literal bytes between the body and the backreference.
    close_pre: Vec<u8>,
    /// Literal bytes after the backreference.
    close_post: Vec<u8>,
}

impl FenceMatcher {
    /// Build a `FenceMatcher` from a recognised [`FenceSpec`].
    /// `prefix` is the global flag prefix (e.g. `"(?i)"`) applied to the tag DFA.
    pub(super) fn build(
        id: SymbolId,
        rank: usize,
        spec: FenceSpec,
        prefix: &str,
    ) -> Result<FenceMatcher, GrammarError> {
        let tag_src = format!("{}{}", prefix, spec.tag_re);
        let tag_dfa = build_combined_dfa(&[&tag_src], MatchKind::LeftmostFirst)?;
        Ok(FenceMatcher {
            id,
            rank,
            open: spec.open,
            tag_dfa,
            sep: spec.sep,
            body_min: spec.body_min,
            close_pre: spec.close_pre,
            close_post: spec.close_post,
        })
    }

    /// Try to match starting exactly at byte offset `pos` in `text`. Returns the
    /// matched slice `&text[pos..end]` on success, or `None` if no match.
    pub(super) fn match_at<'t>(&self, text: &'t str, pos: usize) -> Option<&'t str> {
        let bytes = text.as_bytes();
        let n = bytes.len();

        // Phase 1a: open literal.
        if !bytes.get(pos..)?.starts_with(self.open.as_slice()) {
            return None;
        }
        let after_open = pos + self.open.len();

        // Phase 1b: tag DFA anchored at after_open. Greedy (leftmost-first
        // longest), like Python's tag match before any backtracking.
        let input = Input::new(text).span(after_open..n).anchored(Anchored::Yes);
        let tag_end = match self.tag_dfa.try_search_fwd(&input) {
            Ok(Some(hm)) => hm.offset(),
            _ => return None,
        };
        // tag_end == after_open is valid (nullable tag, e.g. `=*` with zero `=`).

        // Phase 1c: separator literal.
        if !bytes.get(tag_end..)?.starts_with(self.sep.as_slice()) {
            return None;
        }
        let after_sep = tag_end + self.sep.len();

        // Phase 2: build close_seq and scan for its first occurrence at least
        // `body_min` chars after the separator. The body unit is universal and
        // single-char, so "≥ body_min bytes in" is "≥ body_min chars in" (every
        // char is at least one byte, and a close occurrence in valid UTF-8
        // starts on a char boundary).
        let tag_bytes = &bytes[after_open..tag_end];
        let close_seq_len = self.close_pre.len() + tag_bytes.len() + self.close_post.len();
        if close_seq_len == 0 {
            return None; // degenerate: no closing delimiter at all
        }
        let mut close_seq = Vec::with_capacity(close_seq_len);
        close_seq.extend_from_slice(&self.close_pre);
        close_seq.extend_from_slice(tag_bytes);
        close_seq.extend_from_slice(&self.close_post);

        let remaining = bytes.get(after_sep..).unwrap_or(&[]);
        let found = remaining
            .windows(close_seq_len)
            .enumerate()
            .skip(self.body_min)
            .find(|(_, w)| *w == close_seq.as_slice())
            .map(|(i, _)| i);

        // Charge the forward scan (to the close, or to end-of-input on failure)
        // to the lexer-scaling counter, like any other per-position scan work.
        let scanned = found.map(|f| f + close_seq_len).unwrap_or(remaining.len());
        crate::perf::add_lexer_scan_steps(scanned as u64);

        let end = after_sep + found? + close_seq_len;
        Some(&text[pos..end])
    }
}

/// Extract the raw pattern from a terminal def and try to recognise the fence
/// idiom. Returns `None` for string terminals or patterns that do not match.
pub(super) fn recognize_fence_idiom_from_def(def: &TerminalDef) -> Option<FenceSpec> {
    let raw = match &def.pattern {
        Pattern::Re(p) => p.pattern.as_str(),
        Pattern::Str(_) => return None,
    };
    crate::lookaround::lower::recognize_fence_idiom(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lookaround::lower::recognize_fence_idiom;

    fn matcher(pattern: &str) -> FenceMatcher {
        let spec = recognize_fence_idiom(pattern).expect("pattern must be a fence");
        FenceMatcher::build(SymbolId(1), 0, spec, "").expect("fence must build")
    }

    const BRACKET: &str = r"\[(?P<equal_signs>(=*))\[([\s\S]+?)\](?P=equal_signs)\]";
    const HEREDOC: &str = r"<<(?P<heredoc>[a-zA-Z][a-zA-Z0-9._-]+)\n(?:.|\n)*?(?P=heredoc)";

    #[test]
    fn bracket_argument_matches_python_re() {
        let m = matcher(BRACKET);
        // Python: re.match(BRACKET, s) — same matches, same extents.
        assert_eq!(m.match_at("[[x]]", 0), Some("[[x]]"));
        assert_eq!(m.match_at("[==[ ]==] tail", 0), Some("[==[ ]==]"));
        // Lazy body: first close occurrence wins.
        assert_eq!(m.match_at("[[ ]] ]]", 0), Some("[[ ]]"));
        // The echo must match the tag exactly: `]=]` cannot close `[==[`.
        assert_eq!(
            m.match_at("[==[ x ]=] y ]==]", 0),
            Some("[==[ x ]=] y ]==]")
        );
        // Unterminated: no match.
        assert_eq!(m.match_at("[=[ no close", 0), None);
        // Not at the open literal.
        assert_eq!(m.match_at("x[[y]]", 0), None);
    }

    #[test]
    fn bracket_argument_empty_body_is_rejected_like_python() {
        // Python: `([\s\S]+?)` needs ≥1 body char, so `[[]]` / `[==[]==]` do NOT
        // match — pinned because an earlier draft of the matcher accepted them.
        let m = matcher(BRACKET);
        assert_eq!(m.match_at("[[]]", 0), None);
        assert_eq!(m.match_at("[==[]==]", 0), None);
        // …but one body char is enough.
        assert_eq!(m.match_at("[[]]]", 0), Some("[[]]]")); // body = "]"
        assert_eq!(m.match_at("[==[x]==]", 0), Some("[==[x]==]"));
    }

    #[test]
    fn heredoc_matches_python_re() {
        let m = matcher(HEREDOC);
        let s = "<<EOF\nline one\nline two\nEOF more";
        assert_eq!(m.match_at(s, 0), Some("<<EOF\nline one\nline two\nEOF"));
        // `*?` body: an immediate close is valid (body_min 0)…
        assert_eq!(m.match_at("<<EOT\nEOT", 0), Some("<<EOT\nEOT"));
        // …and the echo may sit mid-line (Python's backref has no anchoring).
        assert_eq!(m.match_at("<<EF\nxEFy", 0), Some("<<EF\nxEF"));
        // Tag must match the tag regex (≥2 chars here).
        assert_eq!(m.match_at("<<X\nX", 0), None);
        // Unterminated heredoc: no match.
        assert_eq!(m.match_at("<<EOF\nno close", 0), None);
    }
}
