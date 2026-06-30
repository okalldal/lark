//! Match-width analysis over the lookaround [`Node`] tree — the single width
//! routine the whole `lookaround` module shares (fixed/min/max char widths,
//! quantifier ranges, the `sre_parse`-faithful escape sizing of #454). Moved verbatim
//! from the former single-file `lower.rs` (issue #478 submodule split); behavior
//! unchanged.

use super::*;

/// Fixed char width of `node` — `Some(w)` iff its min and max match widths are equal,
/// `None` otherwise (variable or unbounded). Used to compute a lookbehind's fixed
/// offset from the match start.
pub(super) fn fixed_width_chars(node: &Node) -> Option<usize> {
    let (lo, hi) = width_range(node);
    match hi {
        Some(h) if h == lo => Some(lo),
        _ => None,
    }
}

/// Maximum char width of `node`, or `None` if unbounded — the lookbehind window size.
pub(super) fn max_width_chars(node: &Node) -> Option<usize> {
    width_range(node).1
}

/// The `(min, max)` match width of `node` in characters; `max` is `None` when
/// unbounded (a `*` / `+` / `{m,}` quantifier). A nested assertion contributes its
/// zero consumed width. This is the single width routine the whole `lookaround` module
/// shares: the classifier's bounded-vs-unbounded verdict and stored assertion width
/// ([`super::super::classify::max_width`]) both delegate here, so the proof bound and the
/// runtime lookbehind window can never drift apart.
pub(crate) fn width_range(node: &Node) -> (usize, Option<usize>) {
    match node {
        Node::Atom(s) => atom_width_range(s),
        Node::Concat(parts) => {
            let mut lo = 0usize;
            let mut hi = Some(0usize);
            for p in parts {
                let (plo, phi) = width_range(p);
                lo = lo.saturating_add(plo);
                hi = match (hi, phi) {
                    (Some(a), Some(b)) => Some(a.saturating_add(b)),
                    _ => None,
                };
            }
            (lo, hi)
        }
        Node::Alt(branches) => {
            let mut lo = usize::MAX;
            let mut hi = Some(0usize);
            for b in branches {
                let (blo, bhi) = width_range(b);
                lo = lo.min(blo);
                hi = match (hi, bhi) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    _ => None,
                };
            }
            (if lo == usize::MAX { 0 } else { lo }, hi)
        }
        Node::Group { body, quant, .. } => {
            let (blo, bhi) = width_range(body);
            apply_quant_range(blo, bhi, quant)
        }
        Node::Assertion { .. } => (0, Some(0)),
    }
}

/// Apply a group/element quantifier to a known `(min, max)` body width.
pub(super) fn apply_quant_range(
    lo: usize,
    hi: Option<usize>,
    quant: &str,
) -> (usize, Option<usize>) {
    let q: Vec<char> = quant.chars().collect();
    match q.first().copied() {
        None => (lo, hi),
        Some('*') => (0, None),
        Some('+') => (lo, None),
        Some('?') => (0, hi),
        Some('{') => match parse_brace(&q, 0) {
            // `{m,}` — unbounded above, at least m·lo.
            Some((m, None, _)) => (lo.saturating_mul(m), None),
            Some((m, Some(n), _)) => (lo.saturating_mul(m), hi.map(|h| h.saturating_mul(n))),
            None => (lo, hi), // a literal `{` that wasn't a quantifier
        },
        _ => (lo, hi),
    }
}

/// `(min, max)` char width of a flat, assertion-free atom run; `max` is `None` if any
/// element is unbounded.
pub(super) fn atom_width_range(atom: &str) -> (usize, Option<usize>) {
    let chars: Vec<char> = atom.chars().collect();
    let mut i = 0usize;
    let mut lo = 0usize;
    let mut hi = Some(0usize);
    while i < chars.len() {
        let c = chars[i];
        let elem_w = match c {
            '\\' => {
                i += 1;
                let n = chars.get(i).copied();
                i += 1;
                // A *multi-char* escape is a single code point, exactly as Python's
                // `sre_parse` sizes it (#454): consume the rest of the escape body so its
                // trailing chars are not re-counted as separate literal atoms, and so a
                // following quantifier binds to the whole escape. Width 1 regardless.
                consume_escape_tail(&chars, &mut i, n);
                match n {
                    Some('b') | Some('B') | Some('A') | Some('z') | Some('Z') | Some('G') => 0,
                    _ => 1,
                }
            }
            '[' => {
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
                1
            }
            '^' | '$' => {
                i += 1;
                0
            }
            _ => {
                i += 1;
                1
            }
        };

        // A quantifier binding to this element.
        let (elo, ehi): (usize, Option<usize>) = match chars.get(i).copied() {
            Some('*') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (0, None)
            }
            Some('+') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (elem_w, None)
            }
            Some('?') => {
                i += 1;
                consume_lazy_marker(&chars, &mut i);
                (0, Some(elem_w))
            }
            Some('{') => {
                if let Some((m, maxrep, consumed)) = parse_brace(&chars, i) {
                    i += consumed;
                    consume_lazy_marker(&chars, &mut i);
                    (
                        elem_w.saturating_mul(m),
                        maxrep.map(|n| elem_w.saturating_mul(n)),
                    )
                } else {
                    (elem_w, Some(elem_w))
                }
            }
            _ => (elem_w, Some(elem_w)),
        };
        lo = lo.saturating_add(elo);
        hi = match (hi, ehi) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            _ => None,
        };
    }
    (lo, hi)
}

/// Advance `*i` past the *body* of a multi-char backslash escape whose leading char is
/// `lead` (the `\` and `lead` itself are already consumed; `*i` points just past `lead`).
/// A multi-char escape (`\xHH`, `\uHHHH`, `\UHHHHHHHH`, `\ooo` octal, `\N{name}`) denotes a
/// **single code point** — Python `sre_parse.getwidth()` sizes it at 1 (#454). Without this,
/// [`atom_width_range`] re-counts the escape's trailing chars (`41` of `\x41`, the `name` of
/// `\N{name}`) as separate width-1 literal atoms, over-counting the escape's char width.
///
/// Mirrors `sre_parse`'s lengths: `\x` reads 2 hex digits, `\u` 4, `\U` 8; `\0`–`\7` an octal
/// run (up to three octal digits, the standard greedy `sre_parse` read); `\N` the `{name}`
/// brace. Single-char escapes (`\n`, `\d`, `\.`, the anchors) have no tail and return
/// unchanged. The reads are *bounded and saturating* — a malformed/short escape (e.g. `\x4`
/// at end of input) simply consumes what is present; sizing stays at 1 either way, so width
/// is robust to a body the upstream screens would reject as a build error.
pub(super) fn consume_escape_tail(chars: &[char], i: &mut usize, lead: Option<char>) {
    let is_hex = |c: char| c.is_ascii_hexdigit();
    let is_oct = |c: char| ('0'..='7').contains(&c);
    let mut take = |n: usize, pred: &dyn Fn(char) -> bool| {
        let mut k = 0;
        while k < n && chars.get(*i).is_some_and(|&c| pred(c)) {
            *i += 1;
            k += 1;
        }
    };
    match lead {
        Some('x') => take(2, &is_hex),
        Some('u') => take(4, &is_hex),
        Some('U') => take(8, &is_hex),
        // `\0`–`\7`: the leading octal digit was already consumed as `lead`; read up to
        // two more octal digits (a full octal escape is at most three digits).
        Some(d) if ('0'..='7').contains(&d) => take(2, &is_oct),
        // `\N{name}`: consume through the closing `}` (or to end on a malformed run).
        Some('N') if chars.get(*i) == Some(&'{') => {
            while *i < chars.len() && chars[*i] != '}' {
                *i += 1;
            }
            if *i < chars.len() {
                *i += 1; // past the '}'
            }
        }
        _ => {}
    }
}

/// Skip a lazy (`?`) / possessive (`+`) marker after a quantifier.
pub(super) fn consume_lazy_marker(chars: &[char], i: &mut usize) {
    if matches!(chars.get(*i), Some('?') | Some('+')) {
        *i += 1;
    }
}

/// Parse a `{m}` / `{m,}` / `{m,n}` brace quantifier at `chars[start] == '{'`.
/// Returns `(min, max, chars_consumed)` where `max` is `None` for the unbounded
/// `{m,}`. Returns `None` if it is not a well-formed quantifier (a literal `{`).
pub(super) fn parse_brace(chars: &[char], start: usize) -> Option<(usize, Option<usize>, usize)> {
    debug_assert_eq!(chars.get(start), Some(&'{'));
    let mut i = start + 1;
    let mut lo = String::new();
    while let Some(&c) = chars.get(i) {
        if c.is_ascii_digit() {
            lo.push(c);
            i += 1;
        } else {
            break;
        }
    }
    if lo.is_empty() {
        return None;
    }
    let min = lo.parse::<usize>().unwrap_or(usize::MAX);
    let max = if chars.get(i) == Some(&',') {
        i += 1;
        let mut hi = String::new();
        while let Some(&c) = chars.get(i) {
            if c.is_ascii_digit() {
                hi.push(c);
                i += 1;
            } else {
                break;
            }
        }
        if hi.is_empty() {
            None // `{m,}`
        } else {
            Some(hi.parse::<usize>().unwrap_or(usize::MAX))
        }
    } else {
        Some(min) // `{m}`
    };
    if chars.get(i) == Some(&'}') {
        Some((min, max, i + 1 - start))
    } else {
        None
    }
}
