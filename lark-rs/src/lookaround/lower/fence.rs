//! The fence idiom (idiom #5): named-backref tag-echo delimited tokens (heredocs,
//! CMake bracket arguments). Recognized **without** the lookaround AST parser (which
//! fails on named backreferences); the compiled [`FenceSpec`] is consumed in
//! `lexer/fence.rs`. Moved verbatim from the former single-file `lower.rs` (issue #478
//! submodule split); behavior unchanged.

// ─── Fence idiom (idiom #5): named-backref tag-echo delimited tokens ───────────
//
// The fence idiom matches languages that use the same run-time tag to open and
// close a delimited span — heredoc / heredoc-indent (HCL2 Terraform) and CMake
// bracket arguments (gersemi). The Python `re` module handles these via named
// capturing groups and backreferences:
//
//   <<(?P<heredoc>[a-zA-Z][a-zA-Z0-9._-]+)\n(?:.|\n)*?(?P=heredoc)
//   \[(?P<equal_signs>(=*))\[([\s\S]+?)\](?P=equal_signs)\]
//
// These patterns are **non-regular** (the `regex` crate and `regex-automata`
// both reject them). However, they are **linear-time** recognisable:
//
//   (1) match the open literal + run the tag DFA anchored at `pos` → capture
//       the tag bytes (e.g. "MARKER" or "====");
//   (2) check the separator literal;
//   (3) build `close_seq = close_pre ++ tag_bytes ++ close_post` and scan the
//       rest for its first occurrence at least `body_min` chars in — a single
//       forward pass.
//
// No backtracking, no quadratic *matching*; one failed attempt still scans the
// remaining input once (the same worst case Python `re` pays for the identical
// lazy-body pattern, so oracle parity holds).
//
// **Reject-when-unsure (the recognizer's contract).** Step (3) reproduces
// Python's lazy `body` semantics only when the body matches *any* character, so
// the recognizer demands a body group whose unit is universal (`[\s\S]`,
// `.|\n`) under a **lazy** quantifier (`*?` → `body_min` 0, `+?` → 1); a greedy
// quantifier means Python takes the *last* close occurrence, a constrained body
// means content must be validated — both are rejected so the matcher never
// silently diverges from the oracle. One residual assumption is documented on
// [`FenceSpec`]: no backtracking between the (greedy) tag and the separator.
//
// [`recognize_fence_idiom`] detects the exact shape without calling the
// lookaround AST parser (which fails on named backreferences). The compiled
// [`FenceSpec`] is consumed in `lexer/fence.rs` to build a `FenceMatcher`.

/// The components of a recognised fence pattern; consumed by `lexer/fence.rs`
/// to build a `FenceMatcher`.
///
/// Assumption baked into the two-phase matcher: the tag DFA matches greedily
/// and the separator is then checked with **no backtracking** into the tag.
/// Python `re` would shrink the tag if that made the separator fit. The three
/// audited wild-bank patterns are immune (the tag's character class cannot
/// match the separator's first byte), and a pattern that does backtrack there
/// simply fails to lex where Python matches — it cannot mis-lex a longer or
/// shorter token. Verifying disjointness automatically needs tag-DFA
/// introspection; revisit if a wild grammar ever trips this.
pub struct FenceSpec {
    /// Literal bytes before the named capture group (e.g. `b"<<"` or `b"["`).
    pub open: Vec<u8>,
    /// The tag regex (content of the named capture group, e.g.
    /// `"[a-zA-Z][a-zA-Z0-9._-]+"` or `"(=*)"`).
    pub tag_re: String,
    /// Literal bytes between the tag and the body (e.g. `b"\n"` or `b"["`).
    pub sep: Vec<u8>,
    /// Minimum number of body characters (0 for a `*?` body, 1 for `+?`).
    pub body_min: usize,
    /// Literal bytes between the body and the backreference (e.g. `b""` or `b"]"`).
    pub close_pre: Vec<u8>,
    /// Literal bytes after the backreference (e.g. `b""` or `b"]"`).
    pub close_post: Vec<u8>,
}

/// Try to recognise the fence idiom in the raw regex pattern `raw`.
///
/// The recognised shape is:
///   `OPEN (?P<NAME>TAG_RE) SEP BODY CLOSE_PRE (?P=NAME) CLOSE_POST`
///
/// where OPEN, SEP, CLOSE_PRE, CLOSE_POST are all pure regex literals
/// (no unescaped metacharacters), BODY is one balanced group whose unit is a
/// universal single character (`[\s\S]`, `[\S\s]`, `.|\n`, `\n|.`) under a lazy
/// `*?`/`+?` quantifier (inside or outside the group), and `(?P=NAME)` is the
/// standard named backreference, appearing exactly once.
///
/// Returns `None` if the pattern does not match this exact shape. Never panics.
pub fn recognize_fence_idiom(raw: &str) -> Option<FenceSpec> {
    // Quick pre-check: must contain a named backreference.
    if !raw.contains("(?P=") {
        return None;
    }

    // Find the first `(?P<` at top level (skipping `\X` and character classes).
    let named_open = scan_for(raw.as_bytes(), b"(?P<")?;

    // Everything before `(?P<` must be a pure literal.
    let open = unescape_regex_literal(&raw[..named_open])?;

    // Extract NAME: alphanumeric/underscore chars between `<` and `>`.
    let name_start = named_open + 4; // skip `(?P<`
    let rest_after_open = raw.get(name_start..)?;
    let gt_offset = rest_after_open.find('>')?;
    let name = &rest_after_open[..gt_offset];
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return None;
    }

    // The `(` of `(?P<NAME>...)` is at `named_open`; find its matching `)`.
    let group_close = find_group_close(raw.as_bytes(), named_open)?;
    let after_name_gt = name_start + gt_offset + 1; // position after `>`
    let tag_re = &raw[after_name_gt..group_close];
    if tag_re.is_empty() {
        return None;
    }

    // After the named group: rest = SEP BODY CLOSE_PRE (?P=NAME) CLOSE_POST
    let after_group = group_close + 1;
    let rest = raw.get(after_group..)?;

    // Find `(?P=NAME)` in the rest — must appear exactly once.
    let backref = format!("(?P={})", name);
    let backref_pos = rest.find(backref.as_str())?;
    if rest[backref_pos + backref.len()..].contains(backref.as_str()) {
        return None; // more than one backref: too complex
    }

    let mid = &rest[..backref_pos];
    let close_post_str = &rest[backref_pos + backref.len()..];

    // CLOSE_POST must be a pure literal.
    let close_post = unescape_regex_literal(close_post_str)?;

    // Parse MID → (sep_str, body_str, close_pre_str).
    let (sep_str, body_str, close_pre_str) = split_mid(mid)?;
    let body_min = universal_lazy_body_min(body_str)?;
    let sep = unescape_regex_literal(sep_str)?;
    let close_pre = unescape_regex_literal(close_pre_str)?;

    Some(FenceSpec {
        open,
        tag_re: tag_re.to_string(),
        sep,
        body_min,
        close_pre,
        close_post,
    })
}

/// Find the first occurrence of the byte-string `pat` in `s`, scanning past
/// `\X` escape sequences and `[...]` character classes (so a `pat` inside an
/// escape or class is not reported). Does not track group depth.
pub(super) fn scan_for(s: &[u8], pat: &[u8]) -> Option<usize> {
    let n = s.len();
    let pn = pat.len();
    let mut i = 0;
    while i + pn <= n {
        if s[i] == b'\\' {
            i += 2;
            continue;
        }
        if s[i] == b'[' {
            i = skip_char_class(s, i);
            continue;
        }
        if s[i..].starts_with(pat) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Skip a `[...]` character class starting at `i` (which must point at `[`).
/// Handles a leading `^` and a literal `]` as the first class char. Returns the
/// index just past the closing `]` (or `s.len()` if unterminated).
pub(super) fn skip_char_class(s: &[u8], i: usize) -> usize {
    let n = s.len();
    let mut i = i + 1;
    if i < n && s[i] == b'^' {
        i += 1;
    }
    // `[]` or `[^]` — a literal `]` as first class char.
    if i < n && s[i] == b']' {
        i += 1;
    }
    while i < n && s[i] != b']' {
        if s[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    if i < n {
        i += 1; // skip `]`
    }
    i
}

/// Find the matching `)` for the `(` at position `pos` in `s`, respecting
/// `\X` escapes, `[...]` character classes, and nested groups. Returns the
/// byte index of the matching `)`, or `None` if unbalanced.
pub(super) fn find_group_close(s: &[u8], pos: usize) -> Option<usize> {
    debug_assert_eq!(s.get(pos), Some(&b'('));
    let n = s.len();
    let mut i = pos + 1;
    let mut depth = 1usize;
    while i < n && depth > 0 {
        match s[i] {
            b'\\' => {
                i += 2;
            }
            b'[' => {
                i = skip_char_class(s, i);
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

/// Split the MID section `SEP BODY CLOSE_PRE` of a fence pattern into
/// `(sep_str, body_str, close_pre_str)`. SEP is the literal prefix before the
/// first unescaped `(`; BODY is one balanced group plus any trailing
/// quantifier; CLOSE_PRE is the literal suffix after the body.
///
/// Returns `None` if the MID section is not this exact shape (a missing body
/// group is rejected: without one, Python requires the close sequence to start
/// *immediately*, which the forward close-scan does not reproduce).
pub(super) fn split_mid(mid: &str) -> Option<(&str, &str, &str)> {
    let s = mid.as_bytes();
    let n = s.len();
    let mut i = 0;

    // SEP: literal chars until the first unescaped `(`.
    while i < n {
        match s[i] {
            b'\\' => {
                if i + 1 >= n {
                    return None;
                }
                i += 2;
            }
            b'(' => break,
            // Unescaped metacharacters other than `(` mean the SEP is not a
            // plain literal → reject.
            b'[' | b'*' | b'+' | b'?' | b'^' | b'$' | b'|' | b')' | b'{' | b'.' => return None,
            _ => i += 1,
        }
    }
    let sep_end = i;
    if i >= n {
        return None; // no body group at all
    }

    // BODY: one balanced `(...)` group plus any trailing quantifier chars
    // (validated by `universal_lazy_body_min`, not here).
    let close_pos = find_group_close(s, i)?;
    let mut j = close_pos + 1;
    while j < n && matches!(s[j], b'*' | b'+' | b'?') {
        j += 1;
    }
    let body = &mid[sep_end..j];
    let close_pre_start = j;

    // CLOSE_PRE: must be a pure literal (no more groups or metacharacters).
    let mut k = j;
    while k < n {
        match s[k] {
            b'\\' => {
                if k + 1 >= n {
                    return None;
                }
                k += 2;
            }
            b'(' | b'[' | b'*' | b'+' | b'?' | b'^' | b'$' | b'|' | b')' | b'{' | b'.' => {
                return None;
            }
            _ => k += 1,
        }
    }

    Some((&mid[..sep_end], body, &mid[close_pre_start..]))
}

/// Validate that `body` is a balanced group whose repetition unit is a
/// universal single character under a **lazy** quantifier, and return the
/// quantifier's minimum (`*?` → 0, `+?` → 1). The quantifier may sit inside
/// the group (gersemi `([\s\S]+?)`) or outside it (hcl2 `(?:.|\n)*?`).
///
/// Anything else — greedy quantifiers (Python would take the *last* close
/// occurrence), bounded `{m,n}` repeats, or a content-constrained unit like
/// `[0-9]` (the close-scan never validates body content) — returns `None`,
/// so the caller rejects the pattern rather than risk a silent divergence
/// from Python's semantics.
pub(super) fn universal_lazy_body_min(body: &str) -> Option<usize> {
    // Strip one balanced group layer: `(X)q` or `(?:X)q` → (`X`, outer `q`).
    let s = body.as_bytes();
    if s.first() != Some(&b'(') {
        return None;
    }
    let close = find_group_close(s, 0)?;
    let outer_quant = &body[close + 1..];
    let mut inner = &body[1..close];
    inner = inner.strip_prefix("?:").unwrap_or(inner);

    let (unit, quant) = if outer_quant.is_empty() {
        // Quantifier inside the group: `([\s\S]+?)`.
        match inner {
            i if i.ends_with("*?") || i.ends_with("+?") => (&i[..i.len() - 2], &i[i.len() - 2..]),
            _ => return None,
        }
    } else {
        // Quantifier outside the group: `(?:.|\n)*?`.
        (inner, outer_quant)
    };

    let min = match quant {
        "*?" => 0,
        "+?" => 1,
        _ => return None, // greedy / bounded / double-quantified → reject
    };
    let universal = matches!(unit, r"[\s\S]" | r"[\S\s]" | ".|\\n" | "\\n|.");
    universal.then_some(min)
}

/// Convert a pure regex literal string (no unescaped metacharacters) to the
/// actual bytes it matches. Returns `None` if `s` contains any unescaped
/// regex metacharacter (`.`, `*`, `+`, `?`, `^`, `$`, `|`, `[`, `]`, `(`,
/// `)`, `{`, `}`).
pub(super) fn unescape_regex_literal(s: &str) -> Option<Vec<u8>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' | '*' | '+' | '?' | '^' | '$' | '|' | '[' | ']' | '(' | ')' | '{' | '}' => {
                return None; // unescaped metacharacter
            }
            '\\' => {
                i += 1;
                if i >= chars.len() {
                    return None;
                }
                let b: u8 = match chars[i] {
                    'n' => b'\n',
                    't' => b'\t',
                    'r' => b'\r',
                    'a' => 0x07,
                    'f' => 0x0c,
                    'v' => 0x0b,
                    '\\' => b'\\',
                    '0' => b'\0',
                    'x' => {
                        // `\xHH` hex escape
                        if i + 2 >= chars.len() {
                            return None;
                        }
                        let h: String = chars[i + 1..=i + 2].iter().collect();
                        let v = u8::from_str_radix(&h, 16).ok()?;
                        i += 2;
                        v
                    }
                    // An escaped ASCII punctuation char is itself (`\[` → `[`).
                    // `\d`/`\w`-style class escapes are NOT literals → reject.
                    c if c.is_ascii_punctuation() || c == ' ' => c as u8,
                    _ => return None, // unrecognised escape in a literal context
                };
                out.push(b);
            }
            c => {
                if c.is_ascii() {
                    out.push(c as u8);
                } else {
                    let mut buf = [0u8; 4];
                    let encoded = c.encode_utf8(&mut buf);
                    out.extend_from_slice(encoded.as_bytes());
                }
            }
        }
        i += 1;
    }
    Some(out)
}
