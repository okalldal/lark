use crate::error::GrammarError;
use regex::Regex;

/// Pattern for matching a terminal — either a fixed string or a regex.
#[derive(Debug, Clone)]
pub enum Pattern {
    Str(PatternStr),
    Re(PatternRe),
}

impl Pattern {
    pub fn as_regex_str(&self) -> &str {
        match self {
            Pattern::Str(p) => &p.escaped,
            Pattern::Re(p) => &p.pattern,
        }
    }

    /// Maximum number of **characters** this pattern can match (`None` = unbounded),
    /// mirroring Python Lark's `Pattern.max_width` (`sre_parse.getwidth()[1]`, which
    /// is `MAXWIDTH`/∞ for an unbounded pattern). This is the load-bearing second key
    /// of the terminal-ordering sort (`lark/lexer.py:583`,
    /// `(-priority, -max_width, -len(value), name)`): a finite regex must sort
    /// *behind* a genuinely-unbounded one, so a maximal greedy match wins (#268, RC5).
    ///
    /// For a regex we first parse its source to a `regex-syntax` HIR and walk it
    /// counting characters. A pattern the `regex` crate's parser rejects but Python
    /// `re` *can* size — a **lowerable-lookaround** terminal (`(?=…)`, `(?<=…)`, …),
    /// whose assertions are zero-width — is sized by the assertion-aware analyzer
    /// ([`crate::lookaround::pattern_max_width`], the analogue of Python's
    /// `get_regexp_width(...)[1]`) so `/a(?=b)/` reports a finite `1` rather than
    /// sorting as unbounded (#360, H5-1). Only a pattern *neither* can size (a genuine
    /// backreference — which never builds a lexer anyway) falls back to `None`
    /// (unbounded), the conservative "sort first" default.
    pub fn max_width(&self) -> Option<usize> {
        match self {
            Pattern::Str(p) => Some(p.value.chars().count()),
            Pattern::Re(p) => match regex_syntax::parse(&p.pattern) {
                // The `regex` crate parses it (no lookaround/backref): walk the HIR.
                // `None` here is a genuinely-unbounded finite-engine pattern (`/a+/`).
                Ok(hir) => hir_max_width_chars(&hir),
                // The `regex` crate rejects it — a lookaround idiom Python sizes
                // finitely via `sre_parse` (assertions zero-width). Size it the same
                // way through the shared assertion-aware width walk; only a pattern the
                // analyzer also cannot parse (a real backref) stays `None`/unbounded.
                Err(_) => crate::lookaround::pattern_max_width(&p.pattern).flatten(),
            },
        }
    }

    /// The raw pattern length Python's terminal-ordering tiebreak uses
    /// (`len(pattern.value)` — the *verbatim* source, since Python stores flags
    /// separately on the `Pattern` and never rewrites the body). Two distinct
    /// length-loss sources have to be undone to match Python here:
    ///
    /// * **Flag wrapper (#268, N2).** lark-rs's loader bakes a terminal's flags into
    ///   the regex string as a scoped group (`(?i:aa)`), so a naive `len()` would count
    ///   the wrapper and give a flagged terminal a phantom rank boost. Stripping the
    ///   whole-pattern flag wrapper restores parity: `/aa/` and `/aa/i` both report 2.
    /// * **Body normalization (#399, H6-1).** `PatternRe::new` runs
    ///   `normalize_python_escapes`, which rewrites `\<\<\<` → `<<<` (6→3) and strips
    ///   `(?#…)` comments *before* storage. Measuring the normalized `pattern` would
    ///   undercount; Python measures the verbatim `/…/` source. So we measure the
    ///   **pre-normalization** `raw` source `PatternRe` retains, not `pattern`.
    ///
    /// The flag-wrapper strip still runs (on the raw source): when flags are baked as a
    /// `(?i:…)` group they sit *outside* the body the normalizer would touch, so the
    /// strip behaves identically on raw and normalized — but raw is what keeps the
    /// body verbatim. `raw_value_len() == len(pattern.value)`.
    pub fn raw_value_len(&self) -> usize {
        match self {
            // A `PatternStr`'s value is the literal text; its `i` flag is stored on
            // the struct, never in `value` — so `chars().count()` is `len(value)`.
            Pattern::Str(p) => p.value.chars().count(),
            Pattern::Re(p) => {
                let (raw, _) = crate::lexer::strip_whole_pattern_flag_wrapper(&p.raw, p.flags);
                raw.chars().count()
            }
        }
    }

    /// A self-contained regex for this pattern, suitable for *inlining* into a
    /// larger pattern (e.g. when terminal `A` references terminal `B`). Any flags
    /// are applied as a *scoped* group `(?flags:…)` so they affect only this
    /// sub-pattern and never leak into the rest of the enclosing regex — unlike
    /// `as_regex_str`, which drops the separately-stored flags entirely.
    pub fn to_inline_regex(&self) -> String {
        match self {
            Pattern::Str(p) if p.ci => format!("(?i:{})", p.escaped),
            Pattern::Str(p) => p.escaped.clone(),
            Pattern::Re(p) => {
                let letters = flag_letters(p.flags);
                if letters.is_empty() {
                    p.pattern.clone()
                } else {
                    format!("(?{letters}:{})", p.pattern)
                }
            }
        }
    }
}

/// Maximum match width of a `regex-syntax` HIR, counted in **characters**
/// (`None` = unbounded). Mirrors Python's `sre_parse.getwidth()[1]`: a `+`/`*`/open
/// `{n,}` repetition is unbounded; a literal counts its code points (so a multibyte
/// literal is *one* char, not its UTF-8 byte length — the HIR's own `maximum_len`
/// reports bytes, which would diverge from Python on non-ASCII); a class is one char;
/// concatenation sums, alternation takes the max, and a lookaround assertion is
/// zero-width.
fn hir_max_width_chars(hir: &regex_syntax::hir::Hir) -> Option<usize> {
    use regex_syntax::hir::HirKind;
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Some(0),
        HirKind::Literal(lit) => Some(
            // HIR literals are UTF-8 bytes; count code points for char-width parity.
            std::str::from_utf8(&lit.0)
                .map(|s| s.chars().count())
                .unwrap_or(lit.0.len()),
        ),
        HirKind::Class(_) => Some(1),
        HirKind::Repetition(r) => match r.max {
            None => None, // unbounded (`+`, `*`, `{n,}`)
            Some(max) => hir_max_width_chars(&r.sub).map(|w| w.saturating_mul(max as usize)),
        },
        HirKind::Capture(c) => hir_max_width_chars(&c.sub),
        HirKind::Concat(subs) => subs
            .iter()
            .map(hir_max_width_chars)
            .try_fold(0usize, |acc, w| w.map(|w| acc.saturating_add(w))),
        HirKind::Alternation(subs) => subs
            .iter()
            .map(hir_max_width_chars)
            .try_fold(0usize, |acc, w| w.map(|w| acc.max(w))),
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // `"a"` and `"a"i` share an escaped form but are distinct patterns.
            (Pattern::Str(a), Pattern::Str(b)) => a.value == b.value && a.ci == b.ci,
            _ => self.as_regex_str() == other.as_regex_str(),
        }
    }
}
impl Eq for Pattern {}

impl std::hash::Hash for Pattern {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_regex_str().hash(state);
    }
}

/// A string-literal pattern — Python Lark's `PatternStr`, including the
/// case-insensitive form (`"literal"i`), which Python keeps as a `PatternStr`
/// with the `i` flag attached rather than converting to a regex. Keeping the
/// type here too is what lets a `"keyword"i` literal participate in the
/// lexer's `unless` keyword retyping and sort with string-pattern width
/// semantics, exactly like its case-sensitive sibling.
#[derive(Debug, Clone)]
pub struct PatternStr {
    pub value: String,
    /// regex-escaped form used when building the combined lexer regex
    pub escaped: String,
    /// case-insensitive (`"..."i`): inlined as `(?i:escaped)`.
    pub ci: bool,
}

impl PatternStr {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let escaped = regex::escape(&value);
        PatternStr {
            value,
            escaped,
            ci: false,
        }
    }

    /// A case-insensitive string literal (`"..."i`).
    pub fn new_ci(value: impl Into<String>) -> Self {
        PatternStr {
            ci: true,
            ..Self::new(value)
        }
    }
}

/// Regex flags (bit-field matching Python's re module flags subset).
pub mod flags {
    pub const IGNORECASE: u32 = 1;
    pub const MULTILINE: u32 = 2;
    pub const DOTALL: u32 = 4;
    pub const VERBOSE: u32 = 8;
}

#[derive(Debug, Clone)]
pub struct PatternRe {
    /// The pattern as the lexer compiles it: Python-`re`-dialect constructs the
    /// `regex` crate spells differently are normalized away (`normalize_python_escapes`
    /// rewrites `\<\<\<` → `<<<`, strips `(?#…)` comments, translates octals). This is
    /// what `max_width` (the 2nd sort key) and every scanner read.
    pub pattern: String,
    /// The **pre-normalization** spelling of the pattern: by default the source handed
    /// to [`PatternRe::new`] (`raw = input` before `normalize_python_escapes` rewrites
    /// it). Python's terminal-ordering tiebreak is `len(pattern.value)` over the verbatim
    /// `/…/` source (#399, H6-1), and body normalization (`\<\<\<` → `<<<`, `(?#…)` strip)
    /// must not change a terminal's rank, so `raw_value_len` measures `raw`, not the
    /// normalized `pattern`. For a terminal whose body is a single `/…/` literal the loader
    /// **overrides** `raw` with the verbatim literal source (the normalized combined regex
    /// it builds `pattern`+`flags` from has already de-escaped the body); a composite
    /// terminal keeps `raw == pattern` — the unchanged, pre-existing measure for that path.
    /// `pattern`/`flags` are independent of `raw`, so the scanner build, `unless` retype,
    /// collision check, and eq/hash are untouched. A `(?i:…)` flag wrapper baked into `raw`
    /// (the composite path) is still stripped by `raw_value_len` (#268, N2).
    pub raw: String,
    pub flags: u32,
}

/// Normalize the Python-`re`-dialect constructs the `regex` crate spells differently
/// (or rejects) into their byte-exact regex-crate equivalents, so a Python-accepted
/// terminal compiles and *matches the same characters*. This is the dialect-translation
/// seam called by [`PatternRe::new`] on every `/…/` terminal source. It is
/// **character-class-aware** (a `[...]` body changes escape semantics) and handles, in
/// order of subtlety:
///
/// * **`\<` / `\>`** — Python treats an escaped punctuation char as that literal
///   everywhere, so `\<` / `\>` mean `<` / `>`; the `regex` crate instead reserves them
///   as **word-boundary escapes** — outside a class `\<\>` is two zero-width assertions
///   that match *nothing* where Python matches `"<>"` (a silent mis-lex), and inside a
///   class they are rejected outright (the wild-bank dotmotif `OPERATOR`'s `[\!=\>\<]`).
///   Rewriting exactly those two to the bare char is semantics-preserving in both
///   dialects.
/// * **`(?#…)` comment groups** (H8) — Python's `re` drops an inline comment; the regex
///   crate has no comment group and leaks a raw `unrecognized flag` parse error. We
///   strip the whole `(?#…)` span (honoring `\)` inside it, as Python's `sre_parse`
///   does) so the surrounding pattern is byte-identical to Python's.
/// * **octal escapes** `\0…`, `\ooo` (H9a) — Python reads `\101` as the octal char
///   `0o101 == 'A'`; the regex crate has no octal escape (it reads `\1` as a
///   backreference and rejects it). We translate a Python octal escape to the crate's
///   `\xHH` hex form, mirroring `sre_parse`'s octal-vs-backref rule **exactly**: a
///   leading `\0` is always octal (up to 3 digits total); a leading `\1`–`\7` is octal
///   only when three octal digits are present (`\123`), otherwise it stays a
///   backreference (`\1`, `\12`) and is left for the existing categorized refusal.
///   Inside a character class every `\0`–`\7` run *is* octal (backrefs are not legal in
///   a class — `_class_escape`).
/// * **`\b` inside a character class** (H9b) — Python reads `[\b]` as the backspace char
///   `\x08` (only *outside* a class is `\b` a word boundary); the regex crate rejects
///   `\b` in a class. We rewrite the in-class `\b` to `\x08`.
/// * **empty-lower-bound quantifier `{,n}`** (#400, H6-2) — Python `re` reads `{,n}` (one
///   or more upper digits, empty lower bound) as `{0,n}` (`re.match(r'a{,3}b','aaab')`
///   matches); the regex crate requires a decimal lower bound and rejects the bare form
///   ("repetition quantifier expects a valid decimal"). We insert the implicit `0`,
///   rewriting `{,n}` → `{0,n}` — outside a class only (inside `[...]` a `{` is a literal)
///   and only on a `base_quantifier_len`-valid `{,n}`. A `{,x}` with a non-digit upper, or
///   an unterminated `{,3`, is a literal brace run in Python and is left byte-exact. The
///   inverted-bound `{m,n}` with `m>n` (`a{3,2}`) is *not* touched — it has a lower bound,
///   so it never matches this shape and stays rejected by both engines. **Scoped to
///   `n ≥ 1`:** the fully-empty `{,}` — which Python reads as `{0,}` (== `*`), *not* a
///   literal — is a distinct divergence tracked in #447 (`base_quantifier_len` itself does
///   not yet recognize `{,}`), deliberately out of this rewrite's scope.
///
/// Every other escape — class-special ones like `\]`, idiom-pinned ones like `[^\/]`
/// (the bundled `lark.REGEXP` shape), and `\b`/`\B` *outside* a class (the parked
/// anchor-policy fork, #275) — is left byte-exact.
fn normalize_python_escapes(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    // Whether the scan cursor is inside an unclosed `[...]` character class. Escape
    // semantics (and the very meaning of `\b`, `\1`) differ in and out of a class.
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        // An unescaped `(?#…)` comment group is dropped wholesale (Python `re`). A
        // comment cannot appear inside a character class (`[(?#)]` is a literal class),
        // so only honor it outside one.
        if !in_class && c == '(' && chars.get(i + 1) == Some(&'?') && chars.get(i + 2) == Some(&'#')
        {
            // Skip to the matching `)`, honoring `\)` inside the comment body.
            let mut j = i + 3;
            while j < chars.len() && chars[j] != ')' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            i = j + 1; // past the ')' (or end of input on an unterminated comment)
            continue;
        }
        if c == '\\' {
            let next = chars.get(i + 1).copied();
            match next {
                Some(n @ ('<' | '>')) => {
                    out.push(n); // drop the divergent boundary escape → bare literal
                    i += 2;
                }
                // `[\b]` — backspace inside a class (Python); the crate rejects `\b`
                // here. Outside a class `\b` is the (parked) word-boundary anchor: leave
                // it.
                Some('b') if in_class => {
                    out.push_str("\\x08");
                    i += 2;
                }
                // Octal escape. Outside a class `\0…` is always octal; `\1`–`\7` is
                // octal only as a full 3-octal-digit run (else a backreference, left
                // as-is). Inside a class every `\0`–`\7` is octal.
                Some(d @ '0'..='7') => {
                    if let Some((value, consumed)) = python_octal_escape(&chars, i, in_class, d) {
                        // Emit as the crate's two-hex-digit escape (octal ≤ 0o377 < 256).
                        out.push_str(&format!("\\x{value:02X}"));
                        i += consumed;
                    } else {
                        // A backreference (`\1`, `\12`) — not octal; leave byte-exact for
                        // the existing categorized refusal to reject.
                        out.push('\\');
                        out.push(d);
                        i += 2;
                    }
                }
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                    i += 2;
                }
                None => {
                    out.push('\\');
                    i += 1;
                }
            }
            continue;
        }
        // Empty-lower-bound quantifier `{,n}` → `{0,n}` (#400, H6-2). Python `re` reads
        // `{,n}` (n ≥ 1 upper digits) as `{0,n}`, but the regex crate requires a decimal
        // lower bound and rejects the bare form. We supply the implicit `0` so the crate
        // sees the equivalent pattern. Outside a class only (inside `[...]` a `{` is a
        // literal), and only on a `base_quantifier_len`-valid `{,n}` — a `{,x}` with a
        // non-digit upper / unterminated `{,3` stays a literal brace run, as Python reads
        // it. The fully-empty `{,}` (which Python reads as `{0,}` == `*`, not a literal) is
        // out of scope here and tracked in #447. (A `\{` never reaches this branch: the
        // `\\` branch above consumes the escape pair first.)
        if c == '{' && !in_class {
            if let Some(upper_len) = empty_lower_bound_quantifier_upper_len(&chars, i) {
                out.push_str("{0,");
                // Copy the `n}` verbatim (upper digits + closing brace).
                let rest_start = i + 2; // past `{,`
                for &d in &chars[rest_start..rest_start + upper_len + 1] {
                    out.push(d);
                }
                i = rest_start + upper_len + 1; // past the `}`
                continue;
            }
        }
        if c == '[' && !in_class {
            in_class = true;
            out.push(c);
            i += 1;
            // A `]` as the first class member (or first after `^`) is a literal, not the
            // close — copy it through so the close-tracking below doesn't end the class
            // early.
            if chars.get(i) == Some(&'^') {
                out.push('^');
                i += 1;
            }
            if chars.get(i) == Some(&']') {
                out.push(']');
                i += 1;
            }
            continue;
        }
        if c == ']' && in_class {
            in_class = false;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Parse a Python `re` octal escape starting at `chars[start] == '\\'` with the digit
/// `first` at `start+1`, returning `(value, consumed_chars)` for an **in-range** octal
/// (so the caller can re-emit it as `\xHH`), or `None` if it is a backreference (`\1`,
/// `\12` outside a class) to leave untouched. Out-of-range octals (`> 0o377`) are
/// screened out earlier by [`reject_out_of_range_octal`] (Python errors too) and never
/// reach this translation; the cap here is a defensive guard against a silent `\xHH`
/// wrap if that screen is ever bypassed.
fn python_octal_escape(
    chars: &[char],
    start: usize,
    in_class: bool,
    first: char,
) -> Option<(u32, usize)> {
    let (value, consumed) = python_octal_run(chars, start, in_class, first)?;
    (value <= 0o377).then_some((value, consumed))
}

/// The octal *run* (value + char length) Python `re` recognizes at `chars[start] == '\\'`
/// with octal digit `first` — without range-capping, so a caller can inspect the value
/// to raise Python's "outside range" error. Returns `None` for an out-of-class `\1`–`\7`
/// run of fewer than three octal digits (a backreference, never octal).
///
/// Outside a class (`_escape`): `\0…` consumes up to 2 more octal digits (always octal);
/// `\1`–`\7` is octal **only** as a full three-octal-digit run `\ooo`, else a decimal
/// group reference. Inside a class (`_class_escape`): any `\0`–`\7` consumes up to 3
/// octal digits total and is always octal.
fn python_octal_run(
    chars: &[char],
    start: usize,
    in_class: bool,
    first: char,
) -> Option<(u32, usize)> {
    let is_oct = |c: char| ('0'..='7').contains(&c);
    let d1 = chars.get(start + 2).copied();
    let d2 = chars.get(start + 3).copied();
    if in_class || first == '0' {
        // Greedy up-to-3-octal-digit run (always octal in both cases).
        let mut digits = String::new();
        digits.push(first);
        if let Some(c) = d1 {
            if is_oct(c) {
                digits.push(c);
                if let Some(c2) = d2 {
                    if is_oct(c2) {
                        digits.push(c2);
                    }
                }
            }
        }
        let value = u32::from_str_radix(&digits, 8).ok()?;
        Some((value, 1 + digits.len()))
    } else {
        // `\1`–`\7`: octal only as a full three-octal-digit run.
        match (d1, d2) {
            (Some(c1), Some(c2)) if is_oct(c1) && is_oct(c2) => {
                let value = u32::from_str_radix(&format!("{first}{c1}{c2}"), 8).ok()?;
                Some((value, 4))
            }
            // Fewer than three octal digits → a backreference, not octal.
            _ => None,
        }
    }
}

/// Reject a Python `re` octal escape whose value exceeds `0o377` — Python's `sre_parse`
/// raises `octal escape value \ooo outside of range 0-0o377`, a *build error*, both in
/// and out of a character class. Without this lark-rs would be more permissive than the
/// oracle (ADR-0017): the raw `\401` slips through the lookaround analyzer's fallback and
/// the terminal builds. Runs on the **raw** source before [`normalize_python_escapes`]
/// translates the in-range octals.
fn reject_out_of_range_octal(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    let mut in_class = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            if let Some(d @ '0'..='7') = chars.get(i + 1).copied() {
                if let Some((value, consumed)) = python_octal_run(&chars, i, in_class, d) {
                    if value > 0o377 {
                        return Err(GrammarError::InvalidRegex {
                            pattern: pattern.to_string(),
                            reason: format!(
                                "octal escape value \\{} outside of range 0-0o377 — Python \
                                 `re` (sre_parse) rejects it; lark-rs matches that rejection \
                                 (ADR-0017).",
                                chars[i + 1..i + consumed].iter().collect::<String>()
                            ),
                        });
                    }
                    i += consumed;
                    continue;
                }
            }
            i += 2; // an ordinary escape pair (or `\` at EOF) — never structure
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            i += 1;
            if chars.get(i) == Some(&'^') {
                i += 1;
            }
            if chars.get(i) == Some(&']') {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// Find the first **global (bodiless) inline flag group** — `(?i)`, `(?ms)`, `(?i-s)`,
/// `(?-i)`, … — anywhere in a terminal's regex source, returning its exact `(?flags)`
/// text. This is the `(?flags)` form that sets flags for the rest of the enclosing
/// expression, as opposed to the *scoped* `(?flags:…)` form (which has a body and a
/// `:`). Python Lark rejects every terminal carrying one: it combines all terminals
/// into one regex, wrapping each pattern, which demotes the flag off position 0 — so
/// `re` raises either `global flags not at the start of the expression` (a leading
/// group) or `Cannot compile token` (a mid-pattern group). Either way the terminal is
/// unusable; lark-rs matches that rejection at build (N3, bounty H2). The scoped
/// `(?flags:…)` form — accepted by both engines — is left untouched.
///
/// The scan honors backslash escapes (a literal `\(` is not a group) and character
/// classes (`[(?i)]` is a class, not a flag group).
fn find_global_inline_flag_group(pattern: &str) -> Option<String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2; // skip the escape pair (a literal metachar, never structure)
            }
            '[' => {
                // Skip a character class verbatim, honoring `\]`, `[^…]`, and a
                // literal `]` as the first member.
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    i += if chars[i] == '\\' { 2 } else { 1 };
                }
                i += 1; // past the closing ']' (or end of input)
            }
            '(' if chars.get(i + 1) == Some(&'?') => {
                // Read flag letters / `-` after "(?". A bodiless flag group ends in
                // ')' with no ':' body; a scoped `(?flags:…)` has a ':' and is fine,
                // and an assertion (`(?=`, `(?!`, `(?<`) or a named group (`(?P<`,
                // `(?<name>`) is not flags-only either (none reach the ')' below).
                let mut j = i + 2;
                let mut saw_flag = false;
                while let Some(&c) = chars.get(j) {
                    if c.is_ascii_alphabetic() || c == '-' {
                        saw_flag = true;
                        j += 1;
                    } else {
                        break;
                    }
                }
                if saw_flag && chars.get(j) == Some(&')') {
                    return Some(chars[i..=j].iter().collect());
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Reject a terminal's *user-authored* regex source that carries a **global (bodiless)
/// inline flag group** — the N3 (bounty H2) parity gate. Called by the grammar loader
/// on each `/…/` regex literal a user writes, *before* it composes into a pattern; the
/// internally-generated `(?i)`-prefixed case-insensitive string-literal bake (`"x"i`)
/// is a `LiteralVal::Str`, never a `LiteralVal::Re`, so it never reaches this gate (it
/// is a Python-supported feature, not the user-authored global flag we reject). See
/// [`find_global_inline_flag_group`].
pub(crate) fn reject_global_inline_flags(pattern: &str) -> Result<(), GrammarError> {
    if let Some(group) = find_global_inline_flag_group(pattern) {
        return Err(GrammarError::InvalidRegex {
            pattern: pattern.to_string(),
            reason: format!(
                "global inline flag group `{}` is not supported — Python Lark rejects it \
                 (the combined-regex wrapper moves it off the start of the expression, so \
                 `re` raises \"global flags not at the start\"). Use a scoped flag group \
                 `(?flags:…)` or a terminal-level flag (`/…/i`) instead.",
                group
            ),
        });
    }
    Ok(())
}

/// Reject the two quantifier-shape constructs the Rust `regex` crate accepts with a
/// *different meaning* than Python `re`, so they would otherwise slip past validation
/// (H6/H7):
///
/// * **possessive quantifiers** `*+`, `++`, `?+`, `{m,n}+` (H6) — Python treats the
///   trailing `+` as a possessive (no give-back) modifier; the crate parses it as nested
///   repetition `(a+)+` (greedy) and silently mis-matches. Possessive backtracking is a
///   documented by-design non-goal (`docs/LOOKAROUND_SCOPE.md`), so this is a *categorized
///   refusal* — never a silent greedy reinterpretation.
/// * **stacked quantifiers** `a{2}{3}`, `a**`, `a*{2}`, … (H7) — a base quantifier
///   applied directly to another base quantifier. Python's `sre_parse` raises "multiple
///   repeat"; the crate accepts it. ADR-0017: do not out-permit the oracle.
///
/// The scan is **character-class-aware** (`[a+]` is a literal `+`, `[{2}]` a literal
/// class) and **escape-aware** (`\+`, `\{` are literals). A `{` is a quantifier only when
/// it is a well-formed `{m}` / `{m,}` / `{,n}` / `{m,n}` — Python reads a malformed
/// `{x}` as literal braces (so `a{2}{x}` is *not* a stacked repeat), and we match that.
fn reject_quantifier_dialect_divergence(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    let mut in_class = false;
    // True immediately after a complete base quantifier (`*`/`+`/`?`/`{m,n}`) plus its
    // optional single lazy/possessive modifier — i.e. when the *next* base quantifier
    // would be a "multiple repeat".
    let mut after_quantifier = false;
    while i < chars.len() {
        let c = chars[i];
        // A `(?#…)` comment group is *transparent* to the quantifier-stacking check —
        // Python `re` rejects `a+(?#c)?` as "multiple repeat" exactly as it rejects
        // `a+?` (the comment vanishes but the `?` is still a second repeat), yet accepts
        // `a(?#c)+` (one repeat on `a`). We must run this screen on the **raw** source
        // (before the comment is stripped) and skip the comment span *without* touching
        // `after_quantifier`, so the across-comment stacking is still caught. An
        // unterminated `(?#…` (no closing `)`) is a Python build error.
        if !in_class && c == '(' && chars.get(i + 1) == Some(&'?') && chars.get(i + 2) == Some(&'#')
        {
            let mut j = i + 3;
            while j < chars.len() && chars[j] != ')' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            if j >= chars.len() {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.to_string(),
                    reason: "missing ), unterminated comment — an inline `(?#…)` comment \
                             group has no closing `)`. Python `re` rejects it; lark-rs \
                             matches that rejection (ADR-0017)."
                        .to_string(),
                });
            }
            i = j + 1; // past the ')' — leave `after_quantifier` unchanged (transparent)
            continue;
        }
        if c == '\\' {
            i += 2;
            after_quantifier = false;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            i += 1;
            // A leading `]` (optionally after `^`) is a class member, not the close.
            if chars.get(i) == Some(&'^') {
                i += 1;
            }
            if chars.get(i) == Some(&']') {
                i += 1;
            }
            after_quantifier = false;
            continue;
        }
        // A base quantifier?
        let quant_len = base_quantifier_len(&chars, i);
        if let Some(len) = quant_len {
            if after_quantifier {
                // A base quantifier applied directly to a quantifier → Python "multiple
                // repeat" build error (H7).
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.to_string(),
                    reason: "multiple repeat — a quantifier is applied directly to another \
                             quantifier (e.g. `a{2}{3}` or `a**`). Python `re` (sre_parse) \
                             rejects this as \"multiple repeat\"; lark-rs matches that \
                             rejection (ADR-0017)."
                        .to_string(),
                });
            }
            i += len;
            // At most one trailing modifier: `?` (lazy) or `+` (possessive). A possessive
            // `+` is the documented backtracking-only non-goal (H6).
            match chars.get(i).copied() {
                Some('+') => {
                    return Err(GrammarError::InvalidRegex {
                        pattern: pattern.to_string(),
                        reason: "possessive quantifier (`*+`/`++`/`?+`/`{m,n}+`) is not \
                                 supported — it is a backtracking-only construct, a \
                                 by-design non-goal (docs/LOOKAROUND_SCOPE.md). Python 3.11 \
                                 `re` *accepts* a possessive (no give-back), but the Rust \
                                 regex crate has no possessive and would silently \
                                 reinterpret it as greedy nested repetition `(a+)+` — a \
                                 different match. lark-rs refuses it (a documented \
                                 diverge-and-document narrowing, ADR-0017) rather than \
                                 silently mis-lex."
                            .to_string(),
                    });
                }
                Some('?') => {
                    // Lazy modifier — consume it; a following base quantifier is then a
                    // multiple repeat.
                    i += 1;
                }
                _ => {}
            }
            after_quantifier = true;
            continue;
        }
        after_quantifier = false;
        i += 1;
    }
    Ok(())
}

/// Reject the regex-crate-only escapes that Python `re` has **no syntax for** at all —
/// so the Rust `regex` crate compiles them but Python errors at build, which would make
/// lark-rs more permissive than the oracle (ADR-0017, the unfalsifiable corollary). The
/// `regex` crate's own validation (`Regex::new` in [`PatternRe::new`]) *accepts* each, so
/// this screen must run first. Three surfaces (H4-2, #342):
///
/// * **`\p` / `\P` unicode-property escapes** — `\p{L}`, `\pL`, `\P{L}`, `\P{Greek}`, even a
///   bare `\p`. The regex crate supports Unicode general-category/script classes via
///   `\p{…}` / `\pX`; Python `re` has no `\p` syntax and raises `bad escape \p`/`\P`. Python
///   rejects these *in and out* of a character class and at any position (`[\p{L}]`,
///   `a\pLb`), so we reject every `\p`/`\P` regardless of class context.
/// * **`\x{…}` braced hex** — `\x{41}`, `\x{1F600}`. The regex crate reads a braced hex
///   code point; Python `re`'s `\x` takes *exactly two* hex digits (`\x41`), so `\x{` is an
///   `incomplete escape \x` to it. We reject `\x` followed by `{` (the braced form). A
///   two-digit `\xHH` is left untouched — Python supports it (the negative control).
/// * **`\z` lowercase end-of-text anchor** — the regex crate's `\z` matches end-of-text;
///   Python `re` spells that `\Z` (uppercase) and raises `bad escape \z` for the lowercase
///   form. Python rejects `\z` in and out of a class, so we reject it unconditionally.
///   (`\Z`/`\b`/`\B` — which Python *accepts* — are the parked anchor-policy fork #275 and
///   are deliberately left alone here.)
///
/// The scan is **escape-aware** (it only triggers on a real `\`-escape, never a literal
/// `p`/`x`/`z`) and walks `\…` pairs so a `\\` does not mask the following char. It does
/// **not** otherwise distinguish class context, because all three constructs are rejected
/// by Python identically in and out of `[…]`. Runs on the **raw** source before
/// [`normalize_python_escapes`] (which would not touch these — they are not in its
/// translation set).
fn reject_regex_crate_only_dialect(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' {
            match chars.get(i + 1).copied() {
                Some(esc @ ('p' | 'P')) => {
                    return Err(GrammarError::InvalidRegex {
                        pattern: pattern.to_string(),
                        reason: format!(
                            "`\\{esc}` unicode-property escape (`\\p{{L}}`/`\\pL`/`\\P{{L}}`) is \
                             a Rust `regex`-crate-only construct — Python `re` has no `\\{esc}` \
                             syntax and raises \"bad escape \\{esc}\" at build. lark-rs matches \
                             that rejection (ADR-0017): being more permissive than the oracle is \
                             unfalsifiable.",
                        ),
                    });
                }
                Some('z') => {
                    return Err(GrammarError::InvalidRegex {
                        pattern: pattern.to_string(),
                        reason: "`\\z` end-of-text anchor is a Rust `regex`-crate-only construct \
                                 — Python `re` spells end-of-text `\\Z` (uppercase) and raises \
                                 \"bad escape \\z\" for the lowercase form. lark-rs matches that \
                                 rejection (ADR-0017)."
                            .to_string(),
                    });
                }
                Some('x') if chars.get(i + 2) == Some(&'{') => {
                    return Err(GrammarError::InvalidRegex {
                        pattern: pattern.to_string(),
                        reason:
                            "`\\x{…}` braced-hex escape is a Rust `regex`-crate-only construct \
                                 — Python `re`'s `\\x` takes exactly two hex digits (`\\x41`), so \
                                 `\\x{` is an \"incomplete escape \\x\" at build. Use `\\xHH` (or \
                                 `\\uHHHH`) instead. lark-rs matches Python's rejection (ADR-0017)."
                                .to_string(),
                    });
                }
                Some(_) => i += 2, // an ordinary escape pair — skip both chars
                None => i += 1,    // a trailing backslash
            }
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// Reject the Rust `regex`-crate-only **angle named-group** spelling `(?<name>…)` —
/// Python `re` has no such syntax (it spells a named capture only `(?P<name>…)`) and
/// raises `unknown extension ?<n` at build, but the crate accepts the angle form
/// natively, so `Regex::new` in [`PatternRe::new`] would otherwise let it through and
/// make lark-rs more permissive than the oracle (ADR-0017, the unfalsifiable corollary).
/// H5-6 (#364).
///
/// The trigger is an **unescaped, unclassed** `(?<` whose char after the `<` is a *name*
/// character — i.e. **not** `=` or `!`. The two excluded chars are exactly the lookbehind
/// spellings `(?<=…)` / `(?<!…)`, which Python *accepts* and the lowering supports; those
/// stay exempt. The Python-accepted `(?P<name>…)` form is naturally exempt: its third
/// char after `(?` is `P`, not `<`, so it never matches `(?<`. (`(?'name'…)` is rejected
/// by *both* engines — the crate also rejects the quote spelling — so it is not screened
/// here; the crate's own `Regex::new` rejection covers it.)
///
/// The scan is **escape-aware** (a literal `\(` is not a group open) and
/// **character-class-aware** (`[(?<x>]` is a literal class — Python reads `(?<` inside
/// `[…]` as plain members, so we must not reject it). Runs on the **raw** source before
/// [`normalize_python_escapes`] (which does not touch `(?<…`).
fn reject_regex_crate_angle_named_group(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    let mut in_class = false;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2; // skip the escape pair (`\(`, `\[`, `\\`, …)
                continue;
            }
            '[' if !in_class => {
                in_class = true;
                i += 1;
                // A `]` as the first class member (or first after `^`) is a literal — copy
                // past it so the close-tracking doesn't end the class early.
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                continue;
            }
            ']' if in_class => {
                in_class = false;
                i += 1;
                continue;
            }
            '(' if !in_class
                && chars.get(i + 1) == Some(&'?')
                && chars.get(i + 2) == Some(&'<') =>
            {
                // `(?<` outside a class. Only the lookbehind forms `(?<=` / `(?<!` are
                // valid Python; anything else (`(?<x>…)`, `(?<name>…)`) is the
                // regex-crate-only angle named group Python rejects.
                match chars.get(i + 3).copied() {
                    Some('=') | Some('!') => {
                        i += 3; // lookbehind — leave it for the lowering path
                        continue;
                    }
                    _ => {
                        return Err(GrammarError::InvalidRegex {
                            pattern: pattern.to_string(),
                            reason: "`(?<name>…)` angle named-group is a Rust `regex`-crate-only \
                                     spelling — Python `re` names a capture only `(?P<name>…)` \
                                     and raises \"unknown extension ?<\" for the angle form. Use \
                                     `(?P<name>…)` instead. lark-rs matches Python's rejection \
                                     (ADR-0017): being more permissive than the oracle is \
                                     unfalsifiable. (The lookbehind spellings `(?<=…)`/`(?<!…)` \
                                     are unaffected.)"
                                .to_string(),
                        });
                    }
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// Re-bucket the Python-`re` **named-character escape** `\N{NAME}` (`\N{BULLET}` →
/// U+2022). Python `re` *accepts* it (the codepoint named `NAME`), but the Rust `regex`
/// crate has no `\N{}` escape and rejects it ("unrecognized escape sequence"). Because the
/// lookaround analyzer parses `\N{…}` as an *ordinary* escape (no assertion), that crate
/// failure would otherwise reach the refusal seam ([`crate::lexer::route_fancy_only_terminal`])
/// and be **mis-categorized** as `LookaroundScope` / "backtracking-only syntax" — none of
/// which `\N{}` is. Screening it here turns it into a correctly-categorized
/// [`GrammarError::InvalidRegex`], fixing the wrong-taxonomy defect (H5-5, #364).
///
/// This is the **fallback** contract of #364 (re-bucket only): **full support** would
/// translate `\N{NAME}` to its codepoint so the terminal builds and matches like Python,
/// but that needs a vendored Unicode-name→codepoint table (138k+ named codepoints) the
/// `regex`/`regex-syntax` crates do not ship — out of scope for the originating task and
/// tracked as a follow-up in #461. Opposite contract to H4-2 (#342), which *rejects*
/// `\p`/`\x{}`/`\z`.
///
/// The trigger is a real `\N` escape (escape-aware: a `\\N{…}` is an escaped backslash
/// then a literal `N{…}`, *not* a named-character escape) immediately followed by `{` —
/// the braced form. A bare `\N` without a brace is a *different* construct (Python `re`
/// raises "missing {"; the crate reads `\N` as "any char except newline") and is left for
/// the existing validation to handle. Class context is irrelevant: Python accepts
/// `[\N{…}]` too, and the crate rejects it identically, so we re-bucket both. Runs on the
/// **raw** source before [`normalize_python_escapes`] (which does not touch `\N{…}`).
fn reject_named_unicode_escape(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '\\' {
            if chars.get(i + 1) == Some(&'N') && chars.get(i + 2) == Some(&'{') {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.to_string(),
                    reason: "`\\N{NAME}` named-character escape is not supported: Python `re` \
                             accepts it (the codepoint named NAME, e.g. `\\N{BULLET}` → U+2022), \
                             but the Rust `regex` crate has no `\\N{}` escape. Full support needs \
                             a Unicode-name→codepoint table the crate does not ship (tracked in \
                             #461). Use the codepoint directly (`\\u2022`) or a `\\xHH`/`\\uHHHH` \
                             escape instead."
                        .to_string(),
                });
            }
            i += 2; // skip the escape pair so a `\\` cannot mask the following `N`
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// If `chars[i]` opens a **base quantifier** — `*`, `+`, `?`, or a well-formed
/// `{m}`/`{m,}`/`{,n}`/`{m,n}` — return its length in chars; else `None`. A `{` that is
/// not a well-formed bound is a literal brace in Python `re` (so it is not a quantifier).
fn base_quantifier_len(chars: &[char], i: usize) -> Option<usize> {
    match chars.get(i).copied()? {
        '*' | '+' | '?' => Some(1),
        '{' => {
            // Scan `{ digits? (, digits?)? }` — at least one digit somewhere.
            let mut j = i + 1;
            let start_digits = j;
            while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                j += 1;
            }
            let had_lower = j > start_digits;
            let mut had_comma = false;
            if chars.get(j) == Some(&',') {
                had_comma = true;
                j += 1;
                while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
                    j += 1;
                }
            }
            // Valid forms: `{m}`, `{m,}`, `{m,n}`, `{,n}`. Always needs at least one
            // digit (`{}` and `{,}` are literal braces in Python).
            let has_digit = had_lower || (had_comma && j > i + 2);
            if has_digit && chars.get(j) == Some(&'}') {
                Some(j - i + 1)
            } else {
                None // a literal `{` (e.g. `{x}`, `{}`, `{`) — not a quantifier
            }
        }
        _ => None,
    }
}

/// If `chars[i]` opens a Python-`re` **empty-lower-bound quantifier** `{,n}` (no lower
/// bound, `≥1` upper digit, e.g. the `{,3}` in `a{,3}b`), return the length of the
/// upper-bound digit run `n` (so `{,3}` ⇒ `1`); else `None`. Python `re` reads `{,n}` as
/// `{0,n}` (`re.match(r'a{,3}b','aaab')` matches), but the Rust `regex` crate requires a
/// decimal lower bound and rejects the bare form ("repetition quantifier expects a valid
/// decimal"), so `normalize_python_escapes` inserts the implicit `0` to feed the crate
/// the equivalent `{0,n}` (#400, H6-2).
///
/// This is precisely the **`{,n}` (n ≥ 1) subcase** of [`base_quantifier_len`]: it
/// returns `Some` iff `base_quantifier_len` would accept this `{…}` *and* the lower bound
/// is empty with the comma present and ≥1 upper digit. So the rewrite fires exactly on
/// the well-formed empty-lower-bound form and never on a literal `{` (`{x}`, `{`), an
/// `{m,n}` carrying a lower bound, or an open `{m,}`. The caller guarantees we are outside
/// a character class and not after a `\` (a `\{` is a literal brace), matching the
/// class-/escape-awareness the rest of `normalize_python_escapes` enforces.
///
/// **`{,}` is deliberately excluded** (returns `None`): Python reads the fully-empty `{,}`
/// as `{0,}` (== `*`), *not* as a literal brace, but `base_quantifier_len` does not yet
/// recognize it, so rewriting it here would be inconsistent with the rest of the
/// quantifier machinery. That distinct divergence is tracked in #447.
fn empty_lower_bound_quantifier_upper_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i).copied()? != '{' || chars.get(i + 1).copied()? != ',' {
        return None;
    }
    // `{,` — count the upper-bound digits; in scope iff `≥1` digit then `}`. (A `{,}` with
    // zero upper digits is Python's `{0,}` == `*`, but is out of this rewrite's scope —
    // #447; a non-digit upper `{,x}` / unterminated `{,3` is a literal brace run.)
    let upper_start = i + 2;
    let mut j = upper_start;
    while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
    }
    let upper_len = j - upper_start;
    if upper_len > 0 && chars.get(j) == Some(&'}') {
        Some(upper_len)
    } else {
        None
    }
}

impl PatternRe {
    pub fn new(pattern: impl Into<String>, flags: u32) -> Result<Self, GrammarError> {
        let raw = pattern.into();
        // Python-`re`-dialect screens that must run on the **raw** source, *before*
        // `normalize_python_escapes` translates octals and strips `(?#…)` comments. Each
        // rejects a construct the Rust `regex` crate would otherwise accept-with-a-
        // different-meaning (or accept where Python errors), so they cannot rely on the
        // `Regex::new` validation or the lookaround refusal seam below (#333):
        //   * out-of-range octal `\401` (Python "outside range 0-0o377" build error),
        //   * possessive `a++` / stacked `a{2}{3}` quantifiers, and an unterminated
        //     `(?#…` comment (H6/H7/H8) — screened on raw so a comment between two
        //     quantifiers (`a+(?#c)?`) is still caught as a multiple-repeat, exactly as
        //     Python rejects it.
        reject_out_of_range_octal(&raw)?;
        reject_quantifier_dialect_divergence(&raw)?;
        // Reject the regex-crate-only escapes Python `re` has no syntax for
        // (`\p`/`\P` unicode-property, `\x{…}` braced hex, `\z` end-of-text) — the crate
        // accepts each, so `Regex::new` below would let them through (#342, H4-2).
        reject_regex_crate_only_dialect(&raw)?;
        // Reject the regex-crate-only *angle* named-group spelling `(?<name>…)` — Python
        // `re` has only `(?P<name>…)` and errors "unknown extension ?<n", but the crate
        // accepts the angle form, so `Regex::new` below would let it through (H5-6, #364).
        // The lookbehind spellings `(?<=`/`(?<!` stay exempt; only `(?<` + a name char is
        // the divergent capture form.
        reject_regex_crate_angle_named_group(&raw)?;
        // Re-bucket the `\N{NAME}` named-character escape: the crate has no `\N{}`, so
        // `Regex::new` fails and — because the lookaround analyzer parses `\N{…}` as a
        // plain escape — the failure would otherwise route through the lookaround seam and
        // be *mis-labelled* "backtracking-only syntax". Screen it here so it surfaces as a
        // correctly-categorized `InvalidRegex`, not `LookaroundScope`. Python *accepts*
        // `\N{NAME}` (named-character escape → codepoint); full support needs a vendored
        // Unicode-name→codepoint table (138k+ named codepoints) and is tracked in #461
        // (H5-5, #364). The opposite contract to H4-2's reject set.
        reject_named_unicode_escape(&raw)?;
        let pattern = normalize_python_escapes(&raw);
        let flag_prefix = build_flag_prefix(flags);
        let full = format!("{}{}", flag_prefix, pattern);
        // Validate the regex early to surface grammar errors. A pattern the linear
        // `regex` crate rejects may still be a valid *lookaround* pattern (some
        // bundled grammars use lookahead/lookbehind — issue #40); accept it if the
        // lookaround analyzer can parse it, and defer the verdict to the lexer-build
        // routing, which either lowers it into the DFA or refuses it with the
        // categorized scope error (`docs/LOOKAROUND_SCOPE.md`). A pattern neither
        // accepts is a real error, reported with the (more familiar) `regex`-crate
        // message plus a backtracking-syntax hint. Deliberately engine-independent:
        // grammar-load outcomes are identical with and without the `fancy-oracle`
        // test feature.
        if let Err(e) = Regex::new(&full) {
            // Parse the raw pattern (not `full`): the analyzer models the loader's
            // baked flag wrapper via the same parse the routing strip uses.
            // Also accept fence-idiom patterns (named backreferences): the lookaround
            // analyzer correctly cannot parse them, but they are handled by the
            // two-phase `FenceMatcher` at lexer-build time.
            if crate::lookaround::parse(&pattern).is_err()
                && crate::lookaround::lower::recognize_fence_idiom(&pattern).is_none()
            {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.clone(),
                    reason: format!(
                        "{e} (and the lookaround analyzer cannot parse it either; \
                         note that backtracking-only syntax is not supported — see \
                         docs/LOOKAROUND_SCOPE.md)"
                    ),
                });
            }
        }
        Ok(PatternRe {
            pattern,
            raw,
            flags,
        })
    }
}

/// The inline-flag letters (`imsx`) for a flag bitset, in canonical order.
/// Empty when no flags are set.
pub fn flag_letters(flags: u32) -> String {
    let mut s = String::new();
    if flags & flags::IGNORECASE != 0 {
        s.push('i');
    }
    if flags & flags::MULTILINE != 0 {
        s.push('m');
    }
    if flags & flags::DOTALL != 0 {
        s.push('s');
    }
    if flags & flags::VERBOSE != 0 {
        s.push('x');
    }
    s
}

fn build_flag_prefix(flags: u32) -> String {
    let mut s = String::from("(?");
    if flags & flags::IGNORECASE != 0 {
        s.push('i');
    }
    if flags & flags::MULTILINE != 0 {
        s.push('m');
    }
    if flags & flags::DOTALL != 0 {
        s.push('s');
    }
    if flags & flags::VERBOSE != 0 {
        s.push('x');
    }
    if s == "(?)" || s == "(?" {
        return String::new();
    }
    s.push(')');
    s
}

/// A fully-resolved terminal definition.
///
/// Note there is no `filter_out` here: whether a token is dropped from the tree
/// is a property of each *rule-symbol occurrence*, not of the terminal (Python
/// Lark's model). The same terminal can be kept at one rule position and dropped
/// at another — e.g. `start: "a" A` with `A: "a"`, where both lex to `A` but the
/// literal occurrence is filtered and the `A` reference is kept. The per-occurrence
/// flag lives on [`Symbol::Terminal`](super::symbol::Terminal) and is lowered into
/// each rule's keep mask.
#[derive(Debug, Clone)]
pub struct TerminalDef {
    pub name: String,
    pub pattern: Pattern,
    /// Higher priority terminals are tried first in the lexer. Stored `i64` (not
    /// `i32`) so two distinct very-large declared priorities do not saturate to the
    /// same value and tie (#352); Python uses unbounded ints.
    pub priority: i64,
    /// A `%declare`d terminal: it has *no* pattern of its own and is never lexed.
    /// It is interned as a terminal (so rules can reference it and the parse table
    /// reserves a column) but excluded from every scanner; a postlex hook (e.g. an
    /// [`Indenter`](crate::postlex::Indenter)) injects its tokens into the stream.
    /// The `pattern` field carries a never-used placeholder for these.
    pub declared: bool,
    /// Whether Python Lark would represent this terminal as a `PatternStr` (a plain
    /// string literal, `pattern.type == "str"`) rather than a `PatternRE`. lark-rs
    /// compiles *every* named terminal to a regex `Pattern`, so this flag preserves
    /// the distinction Python keeps. It matters for the strict-mode regex-collision
    /// check (issue #35), which — exactly like Python's `_check_regex_collisions` —
    /// only ever compares the regex terminals (`pattern.type == "re"`); string
    /// terminals are disambiguated by the lexer's `unless` retyping, not flagged.
    pub string_type: bool,
}

impl TerminalDef {
    pub fn new(name: impl Into<String>, pattern: Pattern, priority: i64) -> Self {
        TerminalDef {
            name: name.into(),
            pattern,
            priority,
            declared: false,
            string_type: false,
        }
    }

    /// Builder-style setter for [`string_type`](Self::string_type).
    pub fn with_string_type(mut self, string_type: bool) -> Self {
        self.string_type = string_type;
        self
    }

    /// A pattern-less `%declare`d terminal (see [`declared`](Self::declared)). The
    /// placeholder pattern never reaches a lexer — `declared` terminals are filtered
    /// out before any scanner is built.
    pub fn declared(name: impl Into<String>) -> Self {
        TerminalDef {
            name: name.into(),
            pattern: Pattern::Str(PatternStr::new("")),
            priority: 0,
            declared: true,
            string_type: false,
        }
    }
}

impl PartialEq for TerminalDef {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for TerminalDef {}

impl std::hash::Hash for TerminalDef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #360 (H5-1): `Pattern::max_width` sizes a **lowerable-lookaround** terminal — one
    /// the `regex` crate refuses to parse — to its finite consumed width (assertions are
    /// zero-width) through the shared assertion-aware analyzer, exactly as Python's
    /// `get_regexp_width(...)[1]` does, instead of falling back to `None`/unbounded.
    /// Every expected number below was grounded against Python Lark 1.3.1.
    #[test]
    fn lookaround_terminal_max_width_is_finite() {
        let w =
            |src: &str| Pattern::Re(PatternRe::new(src, 0).expect("pattern builds")).max_width();
        // Lookaround terminals: the assertion contributes zero width.
        assert_eq!(
            w("a(?=b)"),
            Some(1),
            "trailing lookahead → consumed `a` only"
        );
        assert_eq!(
            w("(?<=x)a"),
            Some(1),
            "leading lookbehind → consumed `a` only"
        );
        assert_eq!(w("foo(?!bar)"), Some(3), "negative lookahead is zero-width");
        // A bare assertion consumes nothing.
        assert_eq!(w("(?=b)"), Some(0), "pure assertion is zero-width");
        // A plain (parseable) pattern still goes through the HIR walk (#268 path).
        assert_eq!(w("a|zz"), Some(2), "finite alternation");
        assert_eq!(w("aa?"), Some(2), "optional element");
        // A `*`/`+` *outside* the assertion is genuinely unbounded → None, matching
        // Python's MAXWIDTH (the conservative "sort first" key is still correct here).
        assert_eq!(
            w("a*(?=b)"),
            None,
            "unbounded repetition outside the assertion"
        );
        assert_eq!(w("a+"), None, "plain unbounded repetition");
    }

    /// #360: the outer `Option` of [`crate::lookaround::pattern_max_width`] reports
    /// *parseability*. A pattern the assertion-aware front-end cannot parse at all (here
    /// a structurally unbalanced `(`) returns the outer `None`, so `Pattern::max_width`'s
    /// `.flatten()` yields `None` (unbounded) — the conservative "sort first" default —
    /// rather than mistaking the un-parse for a width of 0. A pattern it *can* parse
    /// returns `Some(width)`.
    #[test]
    fn unparseable_pattern_reports_outer_none() {
        // Unbalanced paren: the front-end's `parse` errors, so the outer Option is None.
        assert_eq!(crate::lookaround::pattern_max_width("(a"), None);
        // A parseable lookaround pattern reports its finite width (inner Some).
        assert_eq!(
            crate::lookaround::pattern_max_width("a(?=b)"),
            Some(Some(1))
        );
        // A parseable but genuinely-unbounded pattern reports inner None (unbounded).
        assert_eq!(crate::lookaround::pattern_max_width("a+(?=b)"), Some(None));
    }

    /// N3: a *global* (bodiless) inline flag group is detected anywhere — leading or
    /// mid-pattern — and `reject_global_inline_flags` (the loader gate on user regex
    /// literals) rejects it. A scoped `(?flags:…)` group, an assertion, a named group,
    /// an escaped `\(`, and a flag-looking char class are all left alone (no false
    /// positive). `PatternRe::new` itself does NOT gate — it serves internally-composed
    /// patterns (e.g. the `(?i)foo` case-insensitive string-literal bake) too.
    #[test]
    fn detects_only_bodiless_inline_flag_groups() {
        // Rejected: bodiless flag groups (the global form Python rejects).
        for p in [
            "(?i)abc", "(?ms)x", "(?i-s)y", "(?-i)z", "a(?i)b", "x(?im)y",
        ] {
            assert!(
                find_global_inline_flag_group(p).is_some(),
                "{p:?} should be flagged as a global inline flag group"
            );
            assert!(
                reject_global_inline_flags(p).is_err(),
                "the loader gate must reject {p:?}"
            );
        }
        // Accepted: scoped flag groups, assertions, named groups, escaped parens, and a
        // char class whose contents merely look like a flag group.
        for p in [
            "(?i:abc)",
            "(?-i:abc)",
            "x(?i:y)z",
            "(?=ab)cd",
            "(?!ab)cd",
            "(?P<name>x)",
            "(?<name>x)",
            r"\(?i\)abc", // escaped — not a group at all
            "[(?i)]",     // a character class of literal chars
            "[a-z]+",
        ] {
            assert!(
                find_global_inline_flag_group(p).is_none(),
                "{p:?} must NOT be flagged as a global inline flag group"
            );
            assert!(
                reject_global_inline_flags(p).is_ok(),
                "the loader gate must accept {p:?}"
            );
        }
    }

    /// The N3 gate lives in the loader (on user `/…/` literals), NOT in `PatternRe::new`,
    /// so the internal case-insensitive string-literal bake — `(?i)foo` paired with the
    /// `IGNORECASE` bitset, whose leading `(?i)` is load-bearing because `as_regex_str`
    /// drops the separate flag bitset when the literal is *composed* into a larger regex
    /// — still constructs cleanly through `PatternRe::new`.
    #[test]
    fn pattern_re_new_does_not_gate_the_internal_ci_bake() {
        let p = PatternRe::new("(?i)foo", flags::IGNORECASE).expect("ci bake constructs");
        assert_eq!(
            p.pattern, "(?i)foo",
            "the prefix must survive for as_regex_str composition"
        );
    }

    /// H8/H9 (#333): `normalize_python_escapes` translates the Python-`re` dialect
    /// constructs the regex crate spells differently — `(?#…)` comment, octal escapes,
    /// in-class `\b` backspace — to byte-exact regex-crate equivalents, while leaving
    /// backreferences, out-of-class `\b`, and literal escapes untouched.
    #[test]
    fn normalize_translates_python_re_dialect_escapes() {
        // (?#…) comment stripped (and the surrounding pattern preserved, incl. `\)`).
        assert_eq!(normalize_python_escapes("a(?#c)b"), "ab");
        assert_eq!(normalize_python_escapes("a(?#a\\)b)c"), "ac");
        // Octal escape → \xHH (H9a). `\101` == 'A' == 0x41.
        assert_eq!(normalize_python_escapes("\\101"), "\\x41");
        assert_eq!(normalize_python_escapes("\\0"), "\\x00");
        assert_eq!(normalize_python_escapes("\\07"), "\\x07");
        // 3-octal-digit run for a leading 1–7; a bare \1 / \12 stays a backreference.
        assert_eq!(normalize_python_escapes("\\123"), "\\x53");
        assert_eq!(normalize_python_escapes("\\1"), "\\1");
        assert_eq!(normalize_python_escapes("\\12"), "\\12");
        // In a class, any \0–\7 run is octal, and \b is backspace (H9b).
        assert_eq!(normalize_python_escapes("[\\b]"), "[\\x08]");
        assert_eq!(normalize_python_escapes("[\\101]"), "[\\x41]");
        assert_eq!(normalize_python_escapes("[\\1]"), "[\\x01]");
        // Out of a class, \b is the (parked) word-boundary anchor — left untouched.
        assert_eq!(normalize_python_escapes("a\\bc"), "a\\bc");
        // The existing \< \> normalization still applies; other escapes byte-exact.
        assert_eq!(normalize_python_escapes("\\<\\>"), "<>");
        assert_eq!(normalize_python_escapes("[^\\/]"), "[^\\/]");
    }

    /// H6-2 (#400): `normalize_python_escapes` rewrites the Python-`re` empty-lower-bound
    /// quantifier `{,n}` (n ≥ 1) → `{0,n}` (Python reads `{,n}` as `{0,n}`; the regex
    /// crate rejects the bare form). The rewrite is class-aware (a `{` inside `[...]` is a
    /// literal), escape-aware (a `\{` is a literal brace), and fires only on a
    /// `base_quantifier_len`-valid `{,n}` — a `{,x}` with a non-digit upper, an
    /// unterminated `{,3`, or a lower-bounded `{m,n}` is left byte-exact. The inverted
    /// bound `{3,2}` is left untouched (it has a lower bound, so it never matches this
    /// shape and stays rejected downstream). The fully-empty `{,}` (Python's `{0,}` == `*`)
    /// is out of scope (#447) and currently passes through unchanged.
    #[test]
    fn normalize_rewrites_empty_lower_bound_quantifier() {
        // The bug repro and minimal forms.
        assert_eq!(normalize_python_escapes("a{,3}b"), "a{0,3}b");
        // The rewrite is a pure shape translation, position-agnostic — `{,3}` ⇒ `{0,3}`
        // exactly as a bare `{0,3}` is itself. A leading quantifier with nothing to repeat
        // is then a Python `re` error AND a lark-rs build error alike (the shared, pre-
        // existing "nothing to repeat" mis-categorization is #448) — this only pins the
        // normalization output, not that a standalone `{,3}` is accepted.
        assert_eq!(normalize_python_escapes("{,3}"), "{0,3}");
        assert_eq!(normalize_python_escapes("a{,12}"), "a{0,12}"); // multi-digit upper
                                                                   // Multiple occurrences in one pattern are all rewritten.
        assert_eq!(normalize_python_escapes("a{,2}b{,3}"), "a{0,2}b{0,3}");
        // The other well-formed bound forms are untouched (they already have a lower
        // bound or are open-ended — the regex crate accepts them verbatim).
        assert_eq!(normalize_python_escapes("a{2,3}"), "a{2,3}");
        assert_eq!(normalize_python_escapes("a{2,}"), "a{2,}");
        assert_eq!(normalize_python_escapes("a{3}"), "a{3}");
        // Inverted bound — NOT this shape; left byte-exact (stays rejected by both engines).
        assert_eq!(normalize_python_escapes("a{3,2}b"), "a{3,2}b");
        // Class-aware: a `{,3}` *inside* a character class is a set of literal chars in
        // Python (`{`, `,`, `3`, `}`), not a quantifier — left untouched.
        assert_eq!(normalize_python_escapes("[a{,3}]"), "[a{,3}]");
        // Escape-aware: a `\{` is a literal brace, never a quantifier open.
        assert_eq!(normalize_python_escapes("a\\{,3}"), "a\\{,3}");
        // A non-digit upper (`{,x}`) / unterminated (`{,3`) is a literal brace run in
        // Python — left untouched.
        assert_eq!(normalize_python_escapes("a{,x}b"), "a{,x}b"); // non-digit upper
        assert_eq!(normalize_python_escapes("a{,3"), "a{,3"); // unterminated (no `}`)
                                                              // `{,}` is OUT OF SCOPE here (#447): Python reads it as `{0,}` (== `*`), NOT a
                                                              // literal — but `base_quantifier_len` doesn't yet recognize it, so this rewrite
                                                              // leaves it unchanged (lark-rs then still rejects it; the parity fix is #447).
                                                              // This pins only the *current* normalize pass-through, not an oracle claim.
        assert_eq!(normalize_python_escapes("a{,}b"), "a{,}b");
    }

    /// H6/H7 (#333): the quantifier-shape dialect screen refuses possessive (`a++`) and
    /// stacked (`a{2}{3}`) quantifiers — both constructs the regex crate accepts with a
    /// meaning that diverges from Python — while leaving lazy quantifiers, normal
    /// quantifiers, and literal `+`/`{` (in a class or as a malformed bound) accepted.
    #[test]
    fn quantifier_dialect_screen_matches_python() {
        // Possessive (H6) — refused.
        for p in ["a++", "a*+", "a?+", "a{2}+", "a{2,3}+"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?} is possessive — must be refused"
            );
        }
        // Stacked / multiple-repeat (H7) — refused.
        for p in ["a{2}{3}", "a**", "a*{2}", "a+*", "a?*", "a{2}{3}{4}"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?} is a multiple repeat — must be refused"
            );
        }
        // Possessive on a *group* is refused too (the trailing `+` after `)…` quantifier).
        for p in ["(a)*+", "(a+)++", "(?:a){2}+"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?} is a possessive on a group — must be refused"
            );
        }
        // A `(?#…)` comment is transparent to the multiple-repeat check: Python rejects
        // `a+(?#c)?` (the `?` is a second repeat across the comment) but accepts
        // `a(?#c)+` / `a(?#c)?` (one repeat on `a`).
        for p in ["a+(?#c)?", "a+(?#c)*", "a*(?#c)+", "a{2}(?#c){3}"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?}: a comment between two quantifiers is still a multiple repeat"
            );
        }
        // An unterminated `(?#…` comment is a Python build error.
        for p in ["a(?#noend", "a(?#c"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?}: an unterminated `(?#…` comment must be refused"
            );
        }
        // Accepted: lazy modifiers, plain quantifiers, separated quantifiers, transparent
        // comments around a single quantifier, and literal `+`/`{` (in a class, escaped,
        // or a malformed bound Python reads as a literal brace).
        for p in [
            "a*?",
            "a+?",
            "a??",
            "a{2}?",
            "a+",
            "a*",
            "a?",
            "a{2}",
            "a{2,3}",
            "a{2,}",
            "a{2}a{3}",
            "[a+]",
            "[a{2}]",
            "a\\+",
            "a\\++",
            "a{x}",
            "a{2}{x}",
            "a{}",
            "ab*c",
            "a(?#c)+",
            "a(?#c)?",
            "a(?#c)b",
            "(a)(?#c)+",
            "a(?#a\\)b)+",
        ] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_ok(),
                "{p:?} is a regular/Python-accepted construct — must NOT be refused"
            );
        }
    }

    /// H9a out-of-range (#333): a Python octal escape `> 0o377` (`\401`, `\777`, in or
    /// out of a class) is a Python `re` build error; lark-rs matches that rejection
    /// rather than out-permit the oracle (ADR-0017). In-range octals pass the screen.
    #[test]
    fn out_of_range_octal_is_rejected() {
        for p in ["\\401", "\\777", "[\\401]", "[\\777]"] {
            assert!(
                reject_out_of_range_octal(p).is_err(),
                "{p:?} is an out-of-range octal — must be refused"
            );
        }
        for p in ["\\101", "\\377", "\\0", "[\\377]", "\\1", "[\\b]", "abc"] {
            assert!(
                reject_out_of_range_octal(p).is_ok(),
                "{p:?} is in-range / not octal — must pass"
            );
        }
    }

    /// H4-2 (#342): the regex-crate-only escapes Python `re` has no syntax for —
    /// `\p`/`\P` unicode-property, `\x{…}` braced hex, `\z` end-of-text anchor — are
    /// refused (the crate accepts each, so this screen, not `Regex::new`, is what catches
    /// them), in and out of a character class and at any position. The negative controls —
    /// two-digit `\xHH`, `\Z`/`\b`/`\B` (which Python accepts/parks, #275), and a literal
    /// `p`/`x`/`z` — are left accepted, so the screen does not over-reject.
    #[test]
    fn regex_crate_only_dialect_is_rejected() {
        // Rejected: \p / \P (unicode property), \x{…} (braced hex), \z (end-of-text).
        for p in [
            r"\p{L}+",
            r"\pL+",
            r"\P{L}+",
            r"\P{Greek}",
            r"\p", // bare \p — Python still errors "bad escape \p"
            r"\x{41}",
            r"\x{1F600}",
            r"abc\z",
            // In a character class Python rejects each identically.
            r"[\p{L}]",
            r"[\pL]",
            r"[\P{L}]",
            r"[\x{41}]",
            r"[\za-z]",
            // Mid-pattern / after other constructs.
            r"a\pLb",
            r"foo\zbar",
        ] {
            assert!(
                reject_regex_crate_only_dialect(p).is_err(),
                "{p:?} is a regex-crate-only construct Python `re` rejects — must be refused"
            );
        }
        // Accepted: two-digit hex, the Python-accepted/parked anchors (\Z/\b/\B), a
        // literal (non-escaped) p/x/z, and an escaped backslash before one of them.
        for p in [
            r"\x41", r"[\x41]", r"\Z", // Python *accepts* \Z (the parked anchor fork, #275)
            r"abc\Z", r"\b\B", r"pxz",    // literal letters, no escape
            r"\\p{L}", // escaped backslash then a literal `p{L}` — the `p` is not escaped
            r"\x4a",   // two hex digits (lowercase) — Python accepts
            r"[a-z]+", r"\d+",
        ] {
            assert!(
                reject_regex_crate_only_dialect(p).is_ok(),
                "{p:?} is Python-accepted — must NOT be refused"
            );
        }
    }

    /// H5-6 (#364): the regex-crate-only angle named-group `(?<name>…)` is rejected, but
    /// the Python-accepted forms (the `(?P<name>…)` capture, the `(?<=`/`(?<!` lookbehinds,
    /// and a `(?<` that is not a real unescaped group-open) are not. Oracle: `re.compile`.
    #[test]
    fn regex_crate_angle_named_group_is_rejected() {
        // Rejected: `(?<` + a name char (anything but `=`/`!`) — Python "unknown extension".
        for p in [
            r"(?<x>a)",
            r"(?<name>a)",
            r"(?<_n>a)",
            r"a(?<x>b)c",      // mid-pattern
            r"(?:(?<x>a))",    // nested inside a non-capturing group (still unclassed)
            r"(?<x>a)(?<y>b)", // two of them
        ] {
            assert!(
                reject_regex_crate_angle_named_group(p).is_err(),
                "{p:?} is the regex-crate-only angle named-group Python rejects — must be refused"
            );
        }
        // Accepted (Python compiles each): the `(?P<name>…)` spelling, both lookbehind
        // forms, a `(?<` inside a character class (literal members), an escaped `\(?<`,
        // and plain patterns.
        for p in [
            r"(?P<x>a)",  // Python's named capture — exempt
            r"(?<=a)b",   // lookbehind
            r"(?<!a)b",   // negative lookbehind
            r"a(?<=x)b",  // mid-pattern lookbehind
            r"[(?<x>]",   // inside a class: `(?<` are literal members
            r"\(?<x>a\)", // escaped `(` — not a group open
            r"\\(?<=a)b", // escaped backslash then a real lookbehind — still exempt
            r"(?:abc)+",  // ordinary non-capturing group
            r"(a)(b)",    // ordinary captures
        ] {
            assert!(
                reject_regex_crate_angle_named_group(p).is_ok(),
                "{p:?} is Python-accepted — the angle-named-group screen must NOT refuse it"
            );
        }
    }

    /// H5-5 (#364): the `\N{NAME}` named-character escape is re-bucketed (refused here as
    /// `InvalidRegex`) — the crate has no `\N{}` escape and Python *accepts* it, so full
    /// support is deferred to #461, but this screen at least fixes the wrong-taxonomy
    /// defect (it must not reach the lookaround seam). A bare `\N` (no brace) and an
    /// escaped `\\N{…}` are NOT this escape and are left alone.
    #[test]
    fn named_unicode_escape_is_rebucketed() {
        // The braced named-character escape, in and out of a class, at any position.
        for p in [
            r"\N{BULLET}",
            r"a\N{BULLET}b",
            r"[\N{BULLET}]",
            r"\N{LATIN SMALL LETTER A}",
        ] {
            assert!(
                reject_named_unicode_escape(p).is_err(),
                "{p:?} uses the `\\N{{NAME}}` escape the crate cannot host — must be re-bucketed (#364)"
            );
        }
        // Not the named escape: a bare `\N` (no `{` — a different construct), an escaped
        // backslash before `N{…}` (a literal `N{…}`), and plain patterns.
        for p in [
            r"\Na",    // bare \N, no brace
            r"a\Nb",   // bare \N mid-pattern
            r"\\N{x}", // escaped backslash then literal `N{x}` — not `\N{`
            r"N{2}",   // a literal N then a quantifier — no backslash
            r"[a-z]+",
        ] {
            assert!(
                reject_named_unicode_escape(p).is_ok(),
                "{p:?} is not the `\\N{{NAME}}` escape — must NOT be re-bucketed by this screen"
            );
        }
    }
}
