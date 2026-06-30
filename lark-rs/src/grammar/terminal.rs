use crate::error::GrammarError;
use regex::Regex;

/// One structural step of a regex-source scan, as produced by [`RegexCursor::step`].
/// The cursor has already consumed the step's characters and updated its in-class
/// state by the time it returns, so a caller reacts to the step and loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A `[` (plus an optional leading `^`, plus an optional literal `]` first
    /// member) that *opened* a character class. `span` is the chars consumed (`1`
    /// for a bare `[`, up to `3` for `[^]`). The cursor is now in-class.
    ClassOpen { span: usize },
    /// A `]` that *closed* the current character class. The cursor is now out-of-class.
    ClassClose,
    /// A `\`-escape pair: the backslash plus the escaped char `esc` (`None` for a
    /// lone trailing `\` at end of input). Two chars consumed (one for the trailing
    /// `\`). Emitted in **both** class and out-of-class context — the single place
    /// every screen treats `\x` as a literal pair, never structure.
    Escape { esc: Option<char> },
    /// An ordinary single char (`c`), one char consumed. A consumer that needs the
    /// class context reads [`RegexCursor::in_class`] (unchanged by this step).
    Char { c: char },
}

/// A shared, character-class-aware cursor over a terminal's regex source — the single
/// implementation of the `\`-escape / `[...]` / `[^...]` / leading-`]` tracking the
/// Python-`re`-dialect *screens* (`normalize_python_escapes`, `reject_out_of_range_octal`,
/// `find_global_inline_flag_group`, `reject_quantifier_dialect_divergence`,
/// `reject_regex_crate_only_dialect`) used to hand-roll five times (issue #481), plus the
/// two sibling walkers `strip_screening_comments` and `reject_regex_crate_angle_named_group`
/// (issue #501). Centralizing it makes the class-tracking semantics **identical** across
/// all of them — the whole point, since the duplicated copies were the drift surface.
///
/// The two #501 walkers carry logic the cursor does **not** model and keep it as a thin
/// out-of-class layer over the shared steps: `strip_screening_comments` maintains a verbose
/// **scope stack** (per-`(` push/pop) and removes `(?#…)` / verbose `# …` comment spans;
/// `reject_regex_crate_angle_named_group` inspects `(?<` openers. Both detect those
/// constructs from the backing slice *before* stepping (only out-of-class, where the cursor's
/// class flag is unchanged by a `seek` over the span) and delegate every escape / class /
/// leading-`]` boundary to the cursor — `strip_screening_comments` copying each consumed step
/// verbatim so its output stays byte-identical.
///
/// The cursor advances over `chars` one structural [`Step`] at a time, maintaining the
/// in-class flag with Python's class-boundary rules (a `[` opens a class; a leading `^`
/// and/or a leading `]` are literal class members, not the close; a later `]` closes;
/// `\x` is always a literal pair). A screen that needs to look at a *multi-char*
/// construct (a `(?#…)` comment, a `(?flags)` group, a `{,n}` quantifier, an octal run)
/// peeks at its own backing slice from [`pos`](Self::pos) **before** stepping and, when
/// it consumes the span itself, calls [`seek`](Self::seek) to resume past it — the cursor
/// keeps the in-class flag consistent because such constructs are only honored
/// out-of-class (where the flag is unchanged by the skip).
struct RegexCursor<'a> {
    chars: &'a [char],
    i: usize,
    in_class: bool,
}

impl<'a> RegexCursor<'a> {
    fn new(chars: &'a [char]) -> Self {
        RegexCursor {
            chars,
            i: 0,
            in_class: false,
        }
    }

    /// The current scan index (the position the next [`step`](Self::step) reads from).
    fn pos(&self) -> usize {
        self.i
    }

    /// Whether the cursor is currently inside an unclosed `[...]` character class.
    fn in_class(&self) -> bool {
        self.in_class
    }

    /// Whether any input remains.
    fn at_end(&self) -> bool {
        self.i >= self.chars.len()
    }

    /// Jump the scan index to `i` (a caller having consumed a multi-char span — a
    /// comment, flag group, or quantifier — itself). The in-class flag is unchanged:
    /// such spans are only recognized out-of-class, so skipping them never crosses a
    /// class boundary.
    fn seek(&mut self, i: usize) {
        self.i = i;
    }

    /// Advance one structural step, updating the in-class flag, and return what was
    /// consumed. Caller must ensure `!at_end()`.
    fn step(&mut self) -> Step {
        let c = self.chars[self.i];
        if c == '\\' {
            // An escape pair is a literal in every context — never a class boundary.
            let esc = self.chars.get(self.i + 1).copied();
            self.i += if esc.is_some() { 2 } else { 1 };
            return Step::Escape { esc };
        }
        if self.in_class {
            if c == ']' {
                self.in_class = false;
                self.i += 1;
                return Step::ClassClose;
            }
            self.i += 1;
            return Step::Char { c };
        }
        if c == '[' {
            // Enter a class, consuming the optional leading `^` and the optional
            // literal `]` first member so the close-tracking does not end early.
            self.in_class = true;
            let start = self.i;
            self.i += 1;
            if self.chars.get(self.i) == Some(&'^') {
                self.i += 1;
            }
            if self.chars.get(self.i) == Some(&']') {
                self.i += 1;
            }
            return Step::ClassOpen {
                span: self.i - start,
            };
        }
        self.i += 1;
        Step::Char { c }
    }
}

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
    /// Equality gates on the `Pattern` **variant first** — a `Str` is never equal to a
    /// `Re`, even when they share a regex source (`PatternStr("ab") != PatternRe(/ab/)`).
    /// This mirrors Python Lark's `Pattern.__eq__` (`type(self) == type(other) and …`)
    /// and the active `patterns_equivalent` unification gate (#403/#440): both require a
    /// matching kind. Comparing across kinds through `as_regex_str()` (#467) was a latent
    /// trap — a future `HashMap`/`==` would silently mis-merge a string literal onto a
    /// same-source regex terminal.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // `"a"` and `"a"i` share an escaped form but are distinct patterns.
            (Pattern::Str(a), Pattern::Str(b)) => a.value == b.value && a.ci == b.ci,
            (Pattern::Re(a), Pattern::Re(b)) => a.pattern == b.pattern,
            // Cross-kind (`Str` vs `Re`) is never equal.
            (Pattern::Str(_), Pattern::Re(_)) | (Pattern::Re(_), Pattern::Str(_)) => false,
        }
    }
}
impl Eq for Pattern {}

impl std::hash::Hash for Pattern {
    /// Hashing mixes in a per-variant discriminant **before** the body so a `Str` and a
    /// `Re` with the same regex source land in different buckets — keeping `Hash`
    /// consistent with the variant-first `PartialEq` above (`a == b ⇒ hash(a) == hash(b)`,
    /// and never the reverse collision across kinds). #467.
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            // `ci` distinguishes `"a"` from `"a"i` in `eq`, so it must feed the hash too.
            Pattern::Str(p) => {
                0u8.hash(state);
                p.value.hash(state);
                p.ci.hash(state);
            }
            Pattern::Re(p) => {
                1u8.hash(state);
                p.pattern.hash(state);
            }
        }
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
///   inverted-bound `{m,n}` with `m>n` (`a{3,2}`) is *not* touched here — it has a lower
///   bound, so it never matches this empty-lower-bound shape. It is a *valid* counted
///   repetition spelling that the Rust `regex` crate accepts but Python `re` rejects
///   ("min repeat greater than max repeat"), so it is screened out separately by
///   [`find_inverted_bound_quantifier`] (#534), not by this normalization. **Scoped to
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
    // The shared class-aware cursor (#481): one implementation of the `\`-escape /
    // `[...]` / `[^...]` / leading-`]` tracking. The two out-of-class multi-char
    // constructs this screen rewrites — a `(?#…)` comment and a `{,n}` quantifier — are
    // peeked-and-consumed via `seek` *before* stepping, then the cursor handles the
    // escape pairs and class spans.
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        let c = chars[i];
        // An unescaped `(?#…)` comment group is dropped wholesale (Python `re`). A
        // comment cannot appear inside a character class (`[(?#)]` is a literal class),
        // so only honor it outside one.
        if !cur.in_class()
            && c == '('
            && chars.get(i + 1) == Some(&'?')
            && chars.get(i + 2) == Some(&'#')
        {
            cur.seek(end_of_inline_comment(&chars, i));
            continue;
        }
        // Empty-lower-bound quantifier `{,n}` / fully-empty `{,}` → `{0,n}` / `{0,}`
        // (#400 H6-2; #447). Python `re` reads `{,n}` (n ≥ 0 upper digits) as `{0,n}` — and
        // the fully-empty `{,}` as `{0,}` (== `*`), *not* a literal brace — but the regex
        // crate requires a decimal lower bound and rejects the bare form. We supply the
        // implicit `0` so the crate sees the equivalent pattern. Outside a class only
        // (inside `[...]` a `{` is a literal), and only on a `base_quantifier_len`-valid
        // `{,…}` — a `{,x}` with a non-digit upper / unterminated `{,3` stays a literal
        // brace run, as Python reads it. (A `\{` never reaches this branch: the cursor's
        // escape step consumes the escape pair first.)
        if c == '{' && !cur.in_class() {
            if let Some(upper_len) = empty_lower_bound_quantifier_upper_len(&chars, i) {
                out.push_str("{0,");
                // Copy the `n}` verbatim (upper digits + closing brace).
                let rest_start = i + 2; // past `{,`
                for &d in &chars[rest_start..rest_start + upper_len + 1] {
                    out.push(d);
                }
                cur.seek(rest_start + upper_len + 1); // past the `}`
                continue;
            }
            // Literal `{...}` brace run (#462). An out-of-class `{` that is **not** a
            // well-formed quantifier — `{x}`, `a{x}b`, `{}`, `{ 2}`, `{2 }`, `{a,b}`,
            // `{,x}`, `{2,x}`, an unterminated `a{`, … — is a *literal brace* in Python
            // `re` (`re.compile(r'a{x}b')` matches the literal text `a{x}b`), but the Rust
            // `regex` crate rejects it ("repetition quantifier expects a valid decimal" /
            // "unclosed counted repetition") — and that rejection then routes through the
            // lookaround seam and is *mis-categorized* as `LookaroundScope` (it involves no
            // lookaround/backtracking). We escape the brace (`{` → `\{`) so the crate sees a
            // literal `{`; a bare `}` it already treats as a literal, so only the open brace
            // needs escaping. This builds and matches the literal text exactly like Python
            // (oracle-faithful "support & match", ADR-0017). `base_quantifier_len` is the
            // single quantifier oracle shared with the stacking/nothing-to-repeat screens,
            // so a real quantifier (`{2}`, `{2,3}`, `{2,}`) is left untouched and the
            // empty-lower-bound `{,n}`/`{,}` was already rewritten above. A `\{` never
            // reaches here (the cursor's escape step consumes the escape pair first), and a
            // `{` inside a `[...]` class is already a literal to the crate (the `!in_class`
            // guard leaves it).
            if base_quantifier_len(&chars, i).is_none() {
                out.push_str("\\{");
                cur.seek(i + 1);
                continue;
            }
        }
        match cur.step() {
            Step::Escape { esc } => match esc {
                Some(n @ ('<' | '>')) => out.push(n), // divergent boundary escape → bare literal
                // `[\b]` — backspace inside a class (Python); the crate rejects `\b`
                // here. Outside a class `\b` is the (parked) word-boundary anchor: leave
                // it. (`in_class` was true *before* the step, but the cursor only clears
                // it on a `]` — an escape never changes it — so reading it now is safe.)
                Some('b') if cur.in_class() => out.push_str("\\x08"),
                // Octal escape. Outside a class `\0…` is always octal; `\1`–`\7` is
                // octal only as a full 3-octal-digit run (else a backreference, left
                // as-is). Inside a class every `\0`–`\7` is octal.
                Some(d @ '0'..='7') => {
                    if let Some((value, consumed)) =
                        python_octal_escape(&chars, i, cur.in_class(), d)
                    {
                        // Emit as the crate's two-hex-digit escape (octal ≤ 0o377 < 256).
                        // The cursor's escape step consumed only `\` + the first digit;
                        // advance past the rest of the octal run it did not eat.
                        out.push_str(&format!("\\x{value:02X}"));
                        cur.seek(i + consumed);
                    } else {
                        // A backreference (`\1`, `\12`) — not octal; leave byte-exact for
                        // the existing categorized refusal to reject.
                        out.push('\\');
                        out.push(d);
                    }
                }
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            },
            Step::ClassOpen { span } => {
                // Copy the `[`, optional `^`, optional literal `]` verbatim.
                for &ch in &chars[i..i + span] {
                    out.push(ch);
                }
            }
            Step::ClassClose => out.push(']'),
            Step::Char { c } => out.push(c),
        }
    }
    out
}

/// The index just past the closing `)` of an inline `(?#…)` comment that opens at
/// `chars[start] == '('` (the caller having confirmed `chars[start..start+3] == "(?#"`),
/// honoring `\)` inside the comment body exactly as Python's `sre_parse` does. Returns
/// `chars.len()` for an unterminated comment (no closing `)`). This is the single
/// comment-span rule shared by [`normalize_python_escapes`] (which strips the comment)
/// and [`strip_screening_comments`] (which strips it before the dialect screens), so the
/// two never drift on what a `(?#…)` span covers.
fn end_of_inline_comment(chars: &[char], start: usize) -> usize {
    let mut j = start + 3; // past "(?#"
    while j < chars.len() && chars[j] != ')' {
        j += if chars[j] == '\\' { 2 } else { 1 };
    }
    j + 1 // past the ')' (or one past the end on an unterminated comment)
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
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        let in_class = cur.in_class();
        match cur.step() {
            Step::Escape {
                esc: Some(d @ '0'..='7'),
            } => {
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
                    // The escape step consumed only `\` + the first digit; skip the rest
                    // of the octal run it did not eat (a backref `\1`/`\12` returns `None`
                    // here and the 2-char escape step already covered it).
                    cur.seek(i + consumed);
                }
            }
            // Any other escape pair, class span, or ordinary char — already advanced.
            _ => {}
        }
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
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        // A `(?flags)` group is only meaningful out-of-class (inside `[…]` a `(` is a
        // literal member). Detect it before stepping; the cursor otherwise skips the
        // escape pairs and class spans uniformly.
        if !cur.in_class() && chars[i] == '(' && chars.get(i + 1) == Some(&'?') {
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
            // Not a flag group — advance one char (the `(`) and rescan.
            cur.seek(i + 1);
            continue;
        }
        cur.step();
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
    let mut cur = RegexCursor::new(&chars);
    // True immediately after a complete base quantifier (`*`/`+`/`?`/`{m,n}`) plus its
    // optional single lazy/possessive modifier — i.e. when the *next* base quantifier
    // would be a "multiple repeat".
    let mut after_quantifier = false;
    while !cur.at_end() {
        let i = cur.pos();
        let c = chars[i];
        // A `(?#…)` comment group is *transparent* to the quantifier-stacking check —
        // Python `re` rejects `a+(?#c)?` as "multiple repeat" exactly as it rejects
        // `a+?` (the comment vanishes but the `?` is still a second repeat), yet accepts
        // `a(?#c)+` (one repeat on `a`). We must run this screen on the **raw** source
        // (before the comment is stripped) and skip the comment span *without* touching
        // `after_quantifier`, so the across-comment stacking is still caught. An
        // unterminated `(?#…` (no closing `)`) is a Python build error.
        if !cur.in_class()
            && c == '('
            && chars.get(i + 1) == Some(&'?')
            && chars.get(i + 2) == Some(&'#')
        {
            let end = end_of_inline_comment(&chars, i);
            if end > chars.len() {
                return Err(GrammarError::InvalidRegex {
                    pattern: pattern.to_string(),
                    reason: "missing ), unterminated comment — an inline `(?#…)` comment \
                             group has no closing `)`. Python `re` rejects it; lark-rs \
                             matches that rejection (ADR-0017)."
                        .to_string(),
                });
            }
            cur.seek(end); // past the ')' — leave `after_quantifier` unchanged (transparent)
            continue;
        }
        // A base quantifier? (Only out-of-class — a `+`/`{2}` in `[…]` is a literal.)
        if !cur.in_class() {
            if let Some(len) = base_quantifier_len(&chars, i) {
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
                let mut j = i + len;
                // At most one trailing modifier: `?` (lazy) or `+` (possessive). A possessive
                // `+` is the documented backtracking-only non-goal (H6).
                match chars.get(j).copied() {
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
                        j += 1;
                    }
                    _ => {}
                }
                cur.seek(j);
                after_quantifier = true;
                continue;
            }
        }
        // Not a comment or quantifier: let the cursor consume the escape pair, class
        // span, or ordinary char. Every such step resets `after_quantifier` (an
        // intervening literal / `\`-escape / class breaks the directly-applied chain;
        // inside a class it is already false, having been cleared on the class open).
        cur.step();
        after_quantifier = false;
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
    // The cursor's class tracking is inert for this screen (all three escapes are
    // rejected identically in and out of `[…]`); we react only to its `Escape` steps,
    // so the `\`-pair walk is the single shared one. The `\x{` check peeks the char
    // *after* the escaped `x` (`pos()` is past the pair once the step returns, so the
    // braced `{` is `chars[i+2]`).
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        if let Step::Escape { esc } = cur.step() {
            match esc {
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
                _ => {} // an ordinary escape pair (or trailing `\`) — already consumed
            }
        }
    }
    Ok(())
}

/// Produce the view of a terminal's regex source that the *semantic* dialect screens
/// ([`reject_regex_crate_angle_named_group`], [`reject_named_unicode_escape`]) must run
/// against: the raw source with every **comment span removed**, so a construct that only
/// *appears* inside a comment (`(?#…(?<x>…)` or `\N{…}` in comment text) is not mistaken
/// for real regex syntax. Python `re` strips comments before interpreting the pattern, so
/// these screens must see the post-comment view to match the oracle (#364 corrective: the
/// staged H5-5/H5-6 screens ran on the raw source and wrongly rejected comment content).
///
/// Two comment forms are removed, both **outside a character class** (in a class `(?#`,
/// `#`, and whitespace are all literal members — Python treats `[#(?<x>]` as plain chars)
/// and both **escape-aware** (a `\#` / `\(` is literal):
///
/// * **`(?#…)` inline comments** — always, regardless of flags; the span is the shared
///   [`end_of_inline_comment`] rule (`normalize_python_escapes` strips the same span).
/// * **`# …`-to-end-of-line comments** — only where the **VERBOSE** flag is in effect. In
///   verbose mode Python ignores an unescaped `#` and everything to the next `\n`.
///
/// VERBOSE reaches a terminal two ways and **both** are tracked: the bitset (`flags` here
/// — the terminal-level `/…/x` flag on a *single-element* body, or a global
/// `g_regex_flags`), and a **scoped inline flag group** `(?x:…)` / `(?x)` baked into the
/// source (a *composite* terminal body has its `/…/x` flag re-emitted as a `(?x:…)`
/// wrapper by `to_inline_regex`, then re-parsed with `flags == 0`). So this walk maintains
/// a verbose **scope stack**: every group push inherits the enclosing verbose bit, a
/// `(?flags)` bodiless group mutates the current scope for its remainder, and a
/// `(?flags:…)` / `(?flags-flags:…)` scoped group applies its (possibly `-x`-cleared)
/// verbose only within its own parentheses (Python's `(?x:(?-x: # not a comment )…)`
/// semantics).
///
/// **Whitespace is preserved verbatim** (not collapsed): under VERBOSE Python does *not*
/// fuse whitespace-separated tokens into a group — `( ?<x>)` is "nothing to repeat" and
/// `(?< x>)` is still the rejected angle form — so a real `(?<`/`\N{` sits exactly where
/// Python sees one, and the contiguous-token screens already match the oracle without any
/// whitespace rewrite. Replaced comment spans collapse to nothing; flag-group syntax, class
/// bodies, and all other characters are copied byte-for-byte. Run on the **raw** source
/// (before `normalize_python_escapes`), the same point the screens already ran.
fn strip_screening_comments(pattern: &str, flags: u32) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(chars.len());
    // Verbose-scope stack: `verbose[last]` is the flag in effect at the cursor. A `(`
    // pushes (inheriting), a `)` pops; a scoped `(?flags:…)` pushes its own adjusted bit,
    // a bodiless `(?flags)` mutates the top in place.
    let mut verbose: Vec<bool> = vec![flags & flags::VERBOSE != 0];
    // Drives the shared class-/escape-aware [`RegexCursor`] (#481/#501): the `\`-escape,
    // `[...]`/`[^...]`, and leading-`]` tracking — exactly the class-boundary edges the
    // five sibling screens already share — are the cursor's job. The comment-removal and
    // verbose-scope-stack logic this walker *additionally* carries runs only out-of-class
    // (`cur.in_class()` is false), detected from the backing slice before the cursor steps;
    // a recognized comment / flag-group span is consumed via `seek` (always out-of-class,
    // so the cursor's class flag stays consistent). Every other step is copied verbatim
    // (escape pair, class span, or ordinary char) so the output stays byte-identical.
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        if !cur.in_class() {
            let c = chars[i];
            // `(?#…)` inline comment — dropped wholesale (shared span rule), in any scope.
            if c == '(' && chars.get(i + 1) == Some(&'?') && chars.get(i + 2) == Some(&'#') {
                cur.seek(end_of_inline_comment(&chars, i));
                continue;
            }
            // Verbose `# …` comment to end-of-line — dropped where verbose is in effect (the
            // `\n`, if any, is kept on the next pass; it is whitespace either way).
            if *verbose.last().unwrap() && c == '#' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '\n' {
                    j += 1;
                }
                cur.seek(j); // resume at the newline (copied next pass) or end of input
                continue;
            }
            if c == '(' {
                // An inline flag group `(?…)` / `(?…:…)` adjusts verbose; any other group
                // `(`, `(?:`, `(?=`, `(?<=`, `(?P<…>`, … just inherits the current scope.
                let cur_verbose = *verbose.last().unwrap();
                if let Some((new_verbose, bodiless, consumed)) =
                    parse_inline_flag_group(&chars, i, cur_verbose)
                {
                    if bodiless {
                        // `(?flags)` — mutate the current scope for its remainder, no push.
                        *verbose.last_mut().unwrap() = new_verbose;
                    } else {
                        // `(?flags:…)` — a new scope with the adjusted verbose bit.
                        verbose.push(new_verbose);
                    }
                    for &ch in &chars[i..i + consumed] {
                        out.push(ch);
                    }
                    cur.seek(i + consumed);
                    continue;
                }
                verbose.push(cur_verbose); // ordinary group — inherit
                out.push(c);
                cur.seek(i + 1);
                continue;
            }
            if c == ')' {
                if verbose.len() > 1 {
                    verbose.pop();
                }
                out.push(c);
                cur.seek(i + 1);
                continue;
            }
        }
        // Not a comment / flag-group / paren handled above: let the cursor consume the
        // escape pair, class span (including its leading `^`/`]` members), class close, or
        // ordinary char, and copy exactly what it consumed verbatim.
        let before = cur.pos();
        cur.step();
        out.extend(&chars[before..cur.pos()]);
    }
    out
}

/// If `chars[start] == '('` opens an **inline flag group** — `(?flags)` (bodiless),
/// `(?flags:` or `(?flags-flags:` or `(?-flags:` (scoped) — return
/// `(verbose_after, bodiless, consumed_chars)`: the VERBOSE bit this group establishes
/// (derived from `current` verbose by applying the group's `+`/`-` flag letters), whether
/// it is the bodiless form (so the caller mutates the current scope rather than pushing a
/// new one), and how many chars the `(?flags…` opener spans (up to and including the `)`
/// for bodiless, or the `:` for scoped). Returns `None` for anything that is not a
/// flag group (`(?:`, `(?=`, `(?<=`, `(?P<…`, `(?<name>`, a bare `(`, …) so the caller
/// treats it as an ordinary inheriting group. The recognized flag letters are `imsxaLu`
/// (Python's set); only `x` is consulted, the rest are accepted-and-ignored.
fn parse_inline_flag_group(
    chars: &[char],
    start: usize,
    current: bool,
) -> Option<(bool, bool, usize)> {
    if chars.get(start) != Some(&'(') || chars.get(start + 1) != Some(&'?') {
        return None;
    }
    let mut j = start + 2;
    let mut verbose = current;
    let mut sign_neg = false;
    let mut saw_letter = false;
    while let Some(&c) = chars.get(j) {
        match c {
            '-' => {
                sign_neg = true;
                j += 1;
            }
            'i' | 'm' | 's' | 'x' | 'a' | 'L' | 'u' => {
                saw_letter = true;
                if c == 'x' {
                    verbose = !sign_neg;
                }
                j += 1;
            }
            ')' => {
                // Bodiless `(?flags)` — requires at least one flag letter; an empty
                // `(?)` is not a flag group (and is a Python error anyway).
                return saw_letter.then_some((verbose, true, j + 1 - start));
            }
            ':' => {
                // Scoped `(?flags:…)` — a `-` with no following letter is still scoped
                // (`(?-x:…)`), but a bare `(?:` (no letters, no sign) is an ordinary
                // non-capturing group, not a flag group.
                if saw_letter || sign_neg {
                    return Some((verbose, false, j + 1 - start));
                }
                return None;
            }
            _ => return None, // not a flag group (`(?=`, `(?<`, `(?P`, `(?'`, …)
        }
    }
    None // ran off the end without closing — not a well-formed flag group
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
/// `[…]` as plain members, so we must not reject it). Runs on the
/// [`strip_screening_comments`] view of the raw source (before `normalize_python_escapes`)
/// so a `(?<` appearing *inside* a `(?#…)` or verbose `# …` comment — comment text, not a
/// group — is already gone and is not mis-rejected (#364 corrective).
fn reject_regex_crate_angle_named_group(pattern: &str) -> Result<(), GrammarError> {
    let chars: Vec<char> = pattern.chars().collect();
    // Drives the shared class-/escape-aware [`RegexCursor`] (#481/#501): the `\`-escape,
    // `[...]`/`[^...]`, and leading-`]` tracking are the cursor's job — this screen only
    // reacts to a `(?<` opener *out-of-class*, detected from the backing slice before the
    // cursor steps (inside `[…]` a `(?<` is a literal class member, which the cursor's
    // `in_class()` flag tells us). `seek` past a recognized lookbehind so the cursor's
    // class flag stays consistent (the skipped span is out-of-class, never a boundary).
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        if !cur.in_class()
            && chars[i] == '('
            && chars.get(i + 1) == Some(&'?')
            && chars.get(i + 2) == Some(&'<')
        {
            // `(?<` outside a class. Only the lookbehind forms `(?<=` / `(?<!` are
            // valid Python; anything else (`(?<x>…)`, `(?<name>…)`) is the
            // regex-crate-only angle named group Python rejects.
            match chars.get(i + 3).copied() {
                Some('=') | Some('!') => {
                    cur.seek(i + 3); // lookbehind — leave it for the lowering path
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
        cur.step();
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
/// the existing validation to handle. Class context is irrelevant *to this screen*:
/// Python accepts `[\N{…}]` too and the crate rejects it identically, so we re-bucket
/// both (the class-awareness needed to keep a `#` inside `[…]` from looking like a verbose
/// comment lives in [`strip_screening_comments`]). Runs on the [`strip_screening_comments`]
/// view of the raw source (before `normalize_python_escapes`) so a `\N{…}` appearing
/// *inside* a `(?#…)` or verbose `# …` comment — comment text, not an escape — is already
/// gone and is not mis-rebucketed (#364 corrective).
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
            // Valid forms: `{m}`, `{m,}`, `{m,n}`, `{,n}`, and the fully-empty `{,}`.
            // Python `re` reads `{,}` as `{0,}` (== `*`), so the comma alone — with no
            // digit on either side — is a well-formed quantifier (#447, sibling of #400's
            // `{,n}`). The bare `{}` (no comma, no digit) stays a literal brace, as Python
            // reads it. So a `{…}` is a quantifier iff it carries a digit *or* a comma.
            let is_quantifier = had_lower || had_comma;
            if is_quantifier && chars.get(j) == Some(&'}') {
                Some(j - i + 1)
            } else {
                None // a literal `{` (e.g. `{x}`, `{}`, `{`) — not a quantifier
            }
        }
        _ => None,
    }
}

/// Detect a Python-`re` **"min repeat greater than max repeat"** shape locally, returning
/// the char index of the offending `{m,n}` quantifier open `{`, or `None` if the pattern
/// has none. A counted repetition `{m,n}` with an **inverted bound** (`m > n`, both
/// present — e.g. `a{3,2}`) is a Python `re` *build error* (`sre_parse` raises "min repeat
/// greater than max repeat"), but the Rust `regex` crate **accepts** it (it compiles to an
/// empty/never-matching repeat), so `Regex::new` in [`PatternRe::new`] would let the
/// terminal build — making lark-rs *more permissive than the oracle* (the unfalsifiable
/// direction, ADR-0017). We classify the shape directly so it surfaces as a correctly-
/// categorized `InvalidRegex`, never the misleading `LookaroundScope` refusal (#534).
///
/// Only the **both-bounds-present** `{m,n}` form can be inverted: an open `{m,}` has no
/// upper bound, a `{,n}` / `{m}` cannot have `m > n`, and the empty-lower forms are
/// already `{0,n}`/`{0,}` post-normalization (`0` is never greater). The scan reuses
/// [`base_quantifier_len`] (the shared quantifier oracle, so a literal brace `{x}` / `{}`
/// or a non-quantifier `{` is never inspected) and parses the lower/upper digit runs of a
/// `{m,n}` it accepts.
///
/// Runs on the **normalized** pattern (post [`normalize_python_escapes`]), the same string
/// `Regex::new` validates: `(?#…)` comments are already stripped — so an interior comment
/// that makes Python read the braces as a *literal* (`a{3(?#c),2}` → `a\{3,2}`, which
/// Python accepts) is already escaped to `\{` and carries no quantifier shape here — and
/// `{,n}`/`{,}` are already `{0,n}`/`{0,}`. It is **class- and escape-aware** via the
/// shared [`RegexCursor`]: a `{3,2}` inside `[...]` is a set of literal members and a `\{`
/// is an escaped literal brace — neither is a quantifier.
fn find_inverted_bound_quantifier(pattern: &str) -> Option<usize> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        // A `{m,n}` quantifier is only structural out-of-class (inside `[...]` a `{` is a
        // literal member; a `\{` is consumed as an escape pair by `step()` below).
        if !cur.in_class() && chars[i] == '{' {
            if let Some(len) = base_quantifier_len(&chars, i) {
                if let Some((lower, upper)) = inverted_bound_pair(&chars, i, len) {
                    if decimal_gt(lower, upper) {
                        return Some(i);
                    }
                }
                // A well-formed quantifier (`{m}`, `{m,}`, `{0,n}`, or a non-inverted
                // `{m,n}`) — skip past it; it carries no inverted-bound shape.
                cur.seek(i + len);
                continue;
            }
        }
        cur.step();
    }
    None
}

/// For a `base_quantifier_len`-valid `{…}` of length `len` opening at `chars[i] == '{'`,
/// return the `(lower, upper)` **digit slices** iff it is the **both-bounds-present**
/// `{m,n}` form (a lower digit run, a comma, and an upper digit run) — the only shape that
/// can be inverted. Returns `None` for `{m}` (no comma), `{m,}` (open — no upper), and
/// `{,n}` (no lower; post-normalization these are `{0,n}`, but a `0` lower is handled
/// correctly anyway). The runs are returned as raw digit slices (not parsed integers) so a
/// bound too large for any integer type (Python's own `OverflowError` territory) is still
/// compared correctly by [`decimal_gt`].
///
/// This is exactly the **both-bounds-present** case of the shared [`repeat_count_digit_runs`]
/// parser (one digit-run scan for all `{m}`/`{m,}`/`{m,n}` forms, #545), filtered down so the
/// brace-span parsing lives in a single place — the duplicated scan was a drift surface (the
/// same anti-duplication rationale the shared `RegexCursor` cites).
fn inverted_bound_pair(chars: &[char], i: usize, len: usize) -> Option<(&[char], &[char])> {
    match repeat_count_digit_runs(chars, i, len) {
        (Some(lower), Some(upper)) => Some((lower, upper)),
        _ => None, // `{m}` (no upper), open `{m,}`, or no lower — not the both-bounds form
    }
}

/// Compare two non-negative decimal numbers given as all-ASCII-digit slices, by **magnitude**
/// — correct even for values too large to fit any integer type (`a{99999999999999999999}`),
/// which is exactly the counted-repeat territory both [`decimal_gt`] and
/// [`decimal_ge_maxrepeat`] need (Python's own `OverflowError` range, #534/#545). Leading
/// zeros are stripped first (`007` == `7`, an all-zeros run is `0`), then the longer
/// significant-digit string is the larger number, ties broken lexically (digit order ==
/// numeric order). Slices must be all-ASCII-digit (guaranteed by the brace-span parsers).
fn decimal_cmp(a: &[char], b: &[char]) -> std::cmp::Ordering {
    // Significant-digit tail (leading zeros stripped); an all-zeros run yields an empty tail.
    fn sig(d: &[char]) -> &[char] {
        let nz = d.iter().position(|&c| c != '0').unwrap_or(d.len());
        &d[nz..]
    }
    let (a, b) = (sig(a), sig(b));
    // More significant digits ⇒ larger; equal length ⇒ lexical compare is numeric.
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Whether the non-negative decimal `lower` is strictly greater than `upper` (the inverted
/// `{m,n}` bound relation, #534). A thin magnitude compare over [`decimal_cmp`], so the
/// digit-string comparison logic lives in one place.
fn decimal_gt(lower: &[char], upper: &[char]) -> bool {
    decimal_cmp(lower, upper) == std::cmp::Ordering::Greater
}

/// Python's `sre_constants.MAXREPEAT` (`0xFFFFFFFF`) as its decimal digit slice — the
/// smallest counted-repetition bound `re.compile` rejects with `OverflowError: the repetition
/// number is too large`. A count `== MAXREPEAT` is already too large (`a{4294967295}` raises);
/// `MAXREPEAT - 1` (`a{4294967294}`) is the largest Python accepts. Stored as digit `char`s
/// (matching the bound slices it is compared against) so the magnitude compare never parses a
/// possibly-20-digit, integer-overflowing bound into a fixed-width type. #545.
const PY_MAXREPEAT_DECIMAL: &[char] = &['4', '2', '9', '4', '9', '6', '7', '2', '9', '5'];

/// Whether the non-negative decimal `digits` (an all-ASCII-digit slice) is **>= Python's
/// `MAXREPEAT`** (`0xFFFFFFFF == 4294967295`) — i.e. a repetition count Python `re` rejects
/// as "the repetition number is too large". A magnitude compare over [`decimal_cmp`], so a
/// bound too large for any integer type (`a{99999999999999999999}`, the issue's repro) is
/// flagged correctly. `>=` because MAXREPEAT itself is already rejected by Python. #545.
fn decimal_ge_maxrepeat(digits: &[char]) -> bool {
    decimal_cmp(digits, PY_MAXREPEAT_DECIMAL) != std::cmp::Ordering::Less
}

/// For a [`base_quantifier_len`]-valid `{…}` of length `len` opening at `chars[i] == '{'`,
/// return its **lower** and **upper** bound digit slices as `(lower, upper)` where each is
/// `Some` iff that bound carries digits. Covers every counted form: `{m}` ⇒
/// `(Some(m), None)`, `{m,}` ⇒ `(Some(m), None)`, `{m,n}` ⇒ `(Some(m), Some(n))`, and the
/// post-normalization `{0,n}` ⇒ `(Some("0"), Some(n))`. The runs are raw digit slices (not
/// parsed integers) so a bound too large for any integer type is preserved for the
/// magnitude compare in [`find_oversized_repeat_count`]. Bounded by `len`, so no
/// end-of-input check is needed. #545.
fn repeat_count_digit_runs(
    chars: &[char],
    i: usize,
    len: usize,
) -> (Option<&[char]>, Option<&[char]>) {
    let end = i + len - 1; // index of the closing `}`
    let mut j = i + 1;
    let lower_start = j;
    while j < end && chars[j].is_ascii_digit() {
        j += 1;
    }
    let lower = (j > lower_start).then_some(&chars[lower_start..j]);
    if chars.get(j) != Some(&',') {
        return (lower, None); // `{m}` — single bound, no comma
    }
    j += 1; // past the comma
    let upper_start = j;
    while j < end && chars[j].is_ascii_digit() {
        j += 1;
    }
    let upper = (j > upper_start).then_some(&chars[upper_start..j]);
    (lower, upper)
}

/// Detect a Python-`re` **"the repetition number is too large"** shape locally, returning
/// the char index of the offending `{…}` quantifier open `{`, or `None` if every count is
/// in range. A counted repetition `{m}` / `{m,}` / `{m,n}` whose lower **or** upper bound
/// is `>= 0xFFFFFFFF` (Python's `sre_constants.MAXREPEAT`) is a Python `re` *build error*
/// (`re.compile('a{1,99999999999999999999}')` → `OverflowError: the repetition number is
/// too large`). Both downstream finite engines disagree in the unfalsifiable direction
/// (ADR-0017): the Rust `regex` crate accepts a count up to its own `u32` cap
/// (`0xFFFFFFFF`), and a count *over* that cap (which the crate rejects) then slips through
/// the lookaround-analyzer fallback in [`PatternRe::new`] (which sizes the brace count as
/// OK) — so without this screen `a{99999999999999999999}` *builds*. We classify the shape
/// directly so it surfaces as a correctly-categorized `InvalidRegex`, never the misleading
/// `LookaroundScope` refusal, and *match Python's rejection* (#545).
///
/// The threshold is `>= MAXREPEAT`: Python rejects the count `== 0xFFFFFFFF` already
/// (`a{4294967295}` raises), and `0xFFFFFFFE` (`a{4294967294}`) is the largest it accepts —
/// grounded against both engines. The compare is the overflow-safe digit-slice
/// [`decimal_ge_maxrepeat`], so a 20-digit bound is still flagged.
///
/// Runs on the **normalized** pattern (post [`normalize_python_escapes`]), the same string
/// `Regex::new` validates: `(?#…)` comments are already stripped and `{,n}`/`{,}` are
/// already `{0,n}`/`{0,}` (a `0` lower is never over the cap). It is **class- and
/// escape-aware** via the shared [`RegexCursor`]: a `{99…}` inside `[...]` is a set of
/// literal members and a `\{` is an escaped literal brace — neither is a quantifier. Reuses
/// [`base_quantifier_len`] (the shared quantifier oracle) so a literal brace `{x}` / `{}` or
/// an unterminated `{` is never inspected.
fn find_oversized_repeat_count(pattern: &str) -> Option<usize> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut cur = RegexCursor::new(&chars);
    while !cur.at_end() {
        let i = cur.pos();
        // A `{m,n}` quantifier is only structural out-of-class (inside `[...]` a `{` is a
        // literal member; a `\{` is consumed as an escape pair by `step()` below).
        if !cur.in_class() && chars[i] == '{' {
            if let Some(len) = base_quantifier_len(&chars, i) {
                let (lower, upper) = repeat_count_digit_runs(&chars, i, len);
                if lower.is_some_and(decimal_ge_maxrepeat)
                    || upper.is_some_and(decimal_ge_maxrepeat)
                {
                    return Some(i);
                }
                // A well-formed in-range quantifier — skip past it.
                cur.seek(i + len);
                continue;
            }
        }
        cur.step();
    }
    None
}

/// If `chars[i]` opens a Python-`re` **empty-lower-bound quantifier** `{,n}` (no lower
/// bound, `≥0` upper digits — including the fully-empty `{,}`, e.g. the `{,3}` in `a{,3}b`
/// or the `{,}` in `a{,}b`), return the length of the upper-bound digit run `n` (so `{,3}`
/// ⇒ `1`, `{,}` ⇒ `0`); else `None`. Python `re` reads `{,n}` as `{0,n}`
/// (`re.match(r'a{,3}b','aaab')` matches) and `{,}` as `{0,}` (== `*`,
/// `re.match(r'a{,}b','aaab')` matches), but the Rust `regex` crate requires a decimal
/// lower bound and rejects the bare form ("repetition quantifier expects a valid
/// decimal"), so `normalize_python_escapes` inserts the implicit `0` to feed the crate the
/// equivalent `{0,n}` / `{0,}` (#400 H6-2; #447).
///
/// This is precisely the **empty-lower-bound subcase** of [`base_quantifier_len`]: it
/// returns `Some` iff `base_quantifier_len` would accept this `{…}` *and* the lower bound
/// is empty with the comma present (`≥0` upper digits). So the rewrite fires exactly on the
/// well-formed empty-lower-bound forms `{,n}` / `{,}` and never on a literal `{` (`{x}`,
/// `{}`, `{`), an `{m,n}` carrying a lower bound, or an open `{m,}`. The caller guarantees
/// we are outside a character class and not after a `\` (a `\{` is a literal brace),
/// matching the class-/escape-awareness the rest of `normalize_python_escapes` enforces.
fn empty_lower_bound_quantifier_upper_len(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i).copied()? != '{' || chars.get(i + 1).copied()? != ',' {
        return None;
    }
    // `{,` — count the upper-bound digits; in scope iff (`≥0` digits) then `}`. A `{,}` with
    // zero upper digits is Python's `{0,}` == `*` (#447); a `{,n}` is `{0,n}` (#400). A
    // non-digit upper `{,x}` / unterminated `{,3` is a literal brace run (returns `None`).
    let upper_start = i + 2;
    let mut j = upper_start;
    while chars.get(j).is_some_and(|c| c.is_ascii_digit()) {
        j += 1;
    }
    let upper_len = j - upper_start;
    if chars.get(j) == Some(&'}') {
        Some(upper_len)
    } else {
        None
    }
}

/// Detect a Python-`re` **"nothing to repeat"** shape locally, returning the char index of
/// the offending quantifier, or `None` if the pattern has none. This is the #506 successor
/// to the brittle `regex`-crate error-message substring check (`e.to_string().contains(
/// "repetition operator missing expression")`): error-message text is not an API, so a
/// future crate upgrade could reword it and silently route the same syntax back through the
/// lookaround/backtracking path. We classify the shape directly instead.
///
/// A base quantifier (`*`, `+`, `?`, `{m,n}` per [`base_quantifier_len`]) is "nothing to
/// repeat" exactly when it has **no preceding repeatable expression** to bind to — i.e. it
/// sits at one of (grounded against Python `re` 3.11, `sre_parse`):
///
/// * the **start** of the pattern (`*a`),
/// * right after an **alternation** bar `|` (`a|*b`),
/// * right after a **group / assertion opener** — a plain `(`, a non-capturing `(?:`, a
///   lookahead `(?=`/`(?!`, a lookbehind `(?<=`/`(?<!`, a named-group open `(?P<name>`, or a
///   scoped flag-group open `(?flags:` — i.e. immediately after the *open* of any group, and
/// * right after a **bodiless inline flag group** `(?flags)` (which consumes nothing
///   repeatable, so `(?i)*a` is "nothing to repeat" just like `*a`).
///
/// Everything else gives the quantifier something to repeat: a literal, a `\`-escape
/// (`\*`/`\d`/a backref), a closed group/assertion `)` (`(?=a)*` and `()*` are both fine in
/// Python — a *closed* assertion is repeatable), a character class `[...]` close, or a
/// `(?P=name)` backreference. A *second* quantifier stacked on a quantifier is Python's
/// "multiple repeat", a distinct error already screened by
/// [`reject_quantifier_dialect_divergence`] on the raw source — so this screen treats a
/// just-consumed quantifier as repeatable and never re-reports it.
///
/// Runs on the **normalized** pattern (post [`normalize_python_escapes`]), the same string
/// `Regex::new` validates: `(?#…)` comments are already stripped (so `(?#c)*a` ⇒ `*a` is
/// caught) and `{,n}`/`{,}` are already `{0,n}`/`{0,}` (#400/#447). It is **class- and
/// escape-aware** via the shared [`RegexCursor`] (#481/#494): a `*`/`+`/`?`/`{0,3}` inside
/// `[...]` is a literal class member (`[*]`, `[{0,3}]`), and a `\*` is an escaped literal —
/// neither is a quantifier. Detecting the shape here means real lookaround/backreferences
/// (`(?=ab)`, `(?<=ab)c`, `(a)\1`) are never re-bucketed: they carry no nothing-to-repeat
/// shape, so this returns `None` and they continue to the lookaround-analyzer fallback.
fn find_nothing_to_repeat(pattern: &str) -> Option<usize> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut cur = RegexCursor::new(&chars);
    // Whether a repeatable expression sits immediately to the left of the cursor. False at
    // the start, after `|`, and after any group/assertion *open* or bodiless flag group;
    // true after a literal/escape/class/closed-group/backref.
    let mut repeatable = false;
    while !cur.at_end() {
        let i = cur.pos();
        let c = chars[i];
        // Out-of-class structure: alternation, group openers, and `)` close. Inside a
        // `[...]` class none of these are structural (a `(`/`|`/`)` is a literal member), so
        // the cursor's class steps below handle the class body as ordinary repeatable text.
        if !cur.in_class() {
            if c == '|' {
                repeatable = false; // an arm starts here — a following quantifier has nothing
                cur.seek(i + 1);
                continue;
            }
            if c == ')' {
                repeatable = true; // a closed group/assertion is itself repeatable (`()*`)
                cur.seek(i + 1);
                continue;
            }
            if c == '(' {
                // Classify the opener. A `(?P=name)` backreference is a complete repeatable
                // atom; a bodiless `(?flags)` group consumes nothing repeatable; every other
                // opener (`(`, `(?:`, `(?=`, `(?!`, `(?<=`, `(?<!`, `(?P<name>`, `(?flags:`)
                // *opens* a group, so a quantifier immediately after it has nothing to repeat.
                let (consumed, after) = classify_group_opener(&chars, i);
                repeatable = after;
                cur.seek(i + consumed);
                continue;
            }
            // A base quantifier out-of-class: nothing to repeat iff nothing repeatable
            // precedes it. (A quantifier *after* a quantifier is "multiple repeat", a
            // distinct error handled elsewhere — we leave `repeatable` true and move on.)
            if let Some(len) = base_quantifier_len(&chars, i) {
                if !repeatable {
                    return Some(i);
                }
                cur.seek(i + len);
                // A trailing lazy `?` (`a*?`) is part of the same quantifier, not a new one.
                if chars.get(cur.pos()) == Some(&'?') {
                    cur.seek(cur.pos() + 1);
                }
                repeatable = true;
                continue;
            }
        }
        // Everything else — an ordinary char, a `\`-escape pair, or a `[...]` class span —
        // is repeatable text *unless* it is a bare zero-width anchor. A quantifier binds to
        // an atom, and Python `re` treats an anchor as "nothing to repeat": an out-of-class
        // `^`/`$` (`^*`, `$*`) and the anchor escapes `\b`/`\B`/`\A`/`\Z` (`\b*`, …) are
        // non-repeatable, so a quantifier immediately after one is flagged. Inside a `[...]`
        // class `^`/`$` are literal members and `\b` is a backspace literal — all repeatable
        // — so this only fires out-of-class (`!cur.in_class()`), which `step()`'s class
        // tracking already maintains (#510). Scoped strictly to the quantifier-binding
        // question; whether lark-rs *supports* `\b`/`\Z` semantics at all is the parked
        // anchor-policy fork (#275) and is untouched here — an *un*-quantified `\bword\b`
        // builds exactly as before.
        let step = cur.step();
        let is_bare_anchor = !cur.in_class()
            && match step {
                Step::Char { c } => c == '^' || c == '$',
                Step::Escape { esc } => matches!(esc, Some('b' | 'B' | 'A' | 'Z')),
                Step::ClassOpen { .. } | Step::ClassClose => false,
            };
        repeatable = !is_bare_anchor;
    }
    None
}

/// Classify a group/assertion opener at `chars[start] == '('` for [`find_nothing_to_repeat`],
/// returning `(consumed_chars, repeatable_after)`: how many chars the *opener token* spans
/// and whether what it established is a complete repeatable atom (so a following quantifier
/// has something to repeat) rather than an open group (so a following quantifier is "nothing
/// to repeat").
///
/// * `(?P=name)` — a named backreference: a complete repeatable atom. Consume through the
///   `)`, report `repeatable = true`.
/// * `(?flags)` / `(?flags:` — a bodiless or scoped inline flag group: neither establishes a
///   repeatable atom on its own (the bodiless form consumes nothing; the scoped form *opens*
///   a body), so a quantifier immediately after is "nothing to repeat". Consume the opener,
///   report `repeatable = false`.
/// * `(?P<name>`, `(?:`, `(?=`, `(?!`, `(?<=`, `(?<!`, or a bare `(` — every form that
///   *opens* a group/assertion body: consume the whole opener prefix and report
///   `repeatable = false`, so the body's first char is evaluated as if at the start of a
///   group.
fn classify_group_opener(chars: &[char], start: usize) -> (usize, bool) {
    // `(?P=name)` named backreference — a complete repeatable atom; skip to past its `)`.
    if chars.get(start + 1) == Some(&'?')
        && chars.get(start + 2) == Some(&'P')
        && chars.get(start + 3) == Some(&'=')
    {
        let mut j = start + 4;
        while j < chars.len() && chars[j] != ')' {
            j += 1;
        }
        return ((j + 1).min(chars.len()) - start, true); // past the ')' (or end) — repeatable
    }
    // An inline flag group `(?flags)` (bodiless) or `(?flags:…)` (scoped). Either way the
    // group itself is not yet a repeatable atom, so a following quantifier has nothing to
    // repeat. (`parse_inline_flag_group`'s `current` verbose bit is irrelevant here; we
    // consult only the consumed length.)
    if let Some((_verbose, _bodiless, consumed)) = parse_inline_flag_group(chars, start, false) {
        return (consumed, false);
    }
    // Every other opener — `(`, `(?:`, `(?=`, `(?!`, `(?<=`, `(?<!`, `(?P<name>` — opens a
    // group/assertion body. Consume the whole opener prefix so the body's first char is the
    // one evaluated against `repeatable == false` (the caller sets it), correctly flagging a
    // quantifier that opens the body (`(?:*a)`, `(?<=*a)`, `(?P<n>*a)`) while the opener's
    // own `?`/`:`/`=`/`<`/`P`/name chars never spuriously trip the quantifier check.
    (group_opener_prefix_len(chars, start), false)
}

/// The length of a group/assertion *opener prefix* at `chars[start] == '('` — the chars up
/// to (not including) where the group **body** begins — for the openers
/// [`classify_group_opener`] treats as "opens a body": `(`, `(?:`, `(?=`, `(?!`, `(?<=`,
/// `(?<!`, `(?P<name>`. Consuming the whole prefix means the body's first char is what is
/// evaluated against `repeatable == false`, so a quantifier opening the body (`(?:*a)`,
/// `(?<=*a)`, `(?P<n>*a)`) is correctly flagged "nothing to repeat", while the prefix's own
/// `?`/`:`/`=`/`<`/`P`/name chars never spuriously trip the quantifier check.
fn group_opener_prefix_len(chars: &[char], start: usize) -> usize {
    // Bare `(` capturing group.
    if chars.get(start + 1) != Some(&'?') {
        return 1;
    }
    match chars.get(start + 2).copied() {
        // `(?:`, `(?=`, `(?!` — three-char prefixes.
        Some(':' | '=' | '!') => 3,
        Some('<') => match chars.get(start + 3).copied() {
            // `(?<=` / `(?<!` lookbehind — four-char prefixes.
            Some('=' | '!') => 4,
            // `(?<name>` angle named group (the crate accepts it; Python rejects it earlier
            // via `reject_regex_crate_angle_named_group`, but be robust): up to `>`.
            _ => prefix_through_angle_close(chars, start),
        },
        // `(?P<name>` named group — up to and including `>`.
        Some('P') if chars.get(start + 3) == Some(&'<') => prefix_through_angle_close(chars, start),
        // Anything else after `(?` (an assertion form not enumerated): conservatively
        // consume just the `(?` so the next char is evaluated fresh.
        _ => 2,
    }
}

/// Length of a `(?P<name>` / `(?<name>` opener prefix through its closing `>` (or to end of
/// input if unterminated).
fn prefix_through_angle_close(chars: &[char], start: usize) -> usize {
    let mut j = start + 1;
    while j < chars.len() && chars[j] != '>' {
        j += 1;
    }
    (j + 1).min(chars.len()) - start // past the `>` (or clamp at end)
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
        // The two *semantic* dialect screens below must not be fooled by a construct that
        // only appears inside a comment, so they run on the **comment-stripped** view of
        // the raw source — Python `re` removes `(?#…)` (and, under VERBOSE, `# …`) comments
        // before interpreting the pattern, so the screens match the oracle only on the
        // post-comment view (#364 corrective: the screens used to run on `raw` and wrongly
        // rejected `(?<x>` / `\N{…}` text inside a `(?#…)` comment). `strip_screening_comments`
        // is class- and escape-aware and preserves whitespace verbatim (Python does not
        // fuse whitespace-separated tokens into a group, so the contiguous-token screens
        // still see a real `(?<`/`\N{` exactly where Python does).
        let screen_src = strip_screening_comments(&raw, flags);
        // Reject the regex-crate-only *angle* named-group spelling `(?<name>…)` — Python
        // `re` has only `(?P<name>…)` and errors "unknown extension ?<n", but the crate
        // accepts the angle form, so `Regex::new` below would let it through (H5-6, #364).
        // The lookbehind spellings `(?<=`/`(?<!` stay exempt; only `(?<` + a name char is
        // the divergent capture form.
        reject_regex_crate_angle_named_group(&screen_src)?;
        // Re-bucket the `\N{NAME}` named-character escape: the crate has no `\N{}`, so
        // `Regex::new` fails and — because the lookaround analyzer parses `\N{…}` as a
        // plain escape — the failure would otherwise route through the lookaround seam and
        // be *mis-labelled* "backtracking-only syntax". Screen it here so it surfaces as a
        // correctly-categorized `InvalidRegex`, not `LookaroundScope`. Python *accepts*
        // `\N{NAME}` (named-character escape → codepoint); full support needs a vendored
        // Unicode-name→codepoint table (138k+ named codepoints) and is tracked in #461
        // (H5-5, #364). The opposite contract to H4-2's reject set.
        reject_named_unicode_escape(&screen_src)?;
        let pattern = normalize_python_escapes(&raw);
        // **"Nothing to repeat" pre-screen (#448, #506).** A leading/dangling quantifier
        // with nothing to repeat before it (`*a`, `+a`, `?a`, `{0,3}`, the
        // post-normalization `{,3}`/`{,}` of #400/#447, `(?#c)*a` after a stripped
        // zero-width comment, `(?:*a)`, …) is a Python `re` "nothing to repeat" build error
        // — a *malformed quantifier*, NOT lookaround or backtracking. We classify the shape
        // **locally** (`find_nothing_to_repeat`, class-/escape-aware via the shared
        // `RegexCursor`) rather than matching the `regex` crate's diagnostic text: an
        // error-message string is not an API, and a future crate upgrade could reword it and
        // silently route the same syntax back through the lookaround/backtracking path
        // below (#506). Running on the normalized `pattern` matches the string `Regex::new`
        // sees: `(?#…)` comments are already stripped and `{,n}`/`{,}` already `{0,n}`/`{0,}`.
        // The build still *rejects* (parity with Python is unchanged); only the
        // category/message is the truthful `InvalidRegex` "nothing to repeat" rather than the
        // misleading `LookaroundScope`/`OutOfScope` "backtracking-only syntax" refusal. Runs
        // BEFORE the lookaround-analyzer fallback for that reason; a genuine
        // lookaround/backref carries no nothing-to-repeat shape, so it is never re-bucketed.
        if find_nothing_to_repeat(&pattern).is_some() {
            return Err(GrammarError::InvalidRegex {
                pattern: pattern.clone(),
                reason: "nothing to repeat — a quantifier (`*`/`+`/`?`/`{m,n}`) has no \
                     preceding expression to repeat (e.g. a leading `*a`/`+a`/`?a`/\
                     `{0,3}`, or a quantifier right after `(`, `(?:`, `|`, or a stripped \
                     `(?#…)` comment). Python `re` rejects this as \"nothing to \
                     repeat\"; lark-rs matches that rejection (ADR-0017)."
                    .to_string(),
            });
        }
        // **"The repetition number is too large" pre-screen (#545).** A counted repetition
        // `{m}`/`{m,}`/`{m,n}` whose lower or upper bound is `>= 0xFFFFFFFF` (Python's
        // `sre_constants.MAXREPEAT`) is a Python `re` build error
        // (`re.compile('a{1,99999999999999999999}')` → `OverflowError: the repetition number
        // is too large`). Both finite engines disagree in the unfalsifiable direction: the
        // Rust `regex` crate accepts up to its own `u32` cap (`0xFFFFFFFF`), and a count
        // *over* that cap (which the crate rejects) then slips through the lookaround-analyzer
        // fallback below (it sizes the brace count as OK) — so without this screen
        // `a{99999999999999999999}` *builds*, more permissive than the oracle (ADR-0017). We
        // classify the shape locally (`find_oversized_repeat_count`, an overflow-safe
        // digit-slice magnitude compare, class-/escape-aware via the shared `RegexCursor`) so
        // it surfaces as the truthful `InvalidRegex`, never the misleading `LookaroundScope`
        // refusal. Runs on the normalized `pattern` ((?#…) comments already stripped, `{,n}`/
        // `{,}` already `{0,n}`/`{0,}` — a `0` lower is never over the cap). Grounded against
        // both engines: Python rejects `>= MAXREPEAT` (`a{4294967295}` raises), `0xFFFFFFFE`
        // is the largest accepted; the regex crate's parse cap is the same `0xFFFFFFFF`.
        //
        // **Ordered before the #534 inverted-bound screen below**, matching Python's own
        // evaluation order: `sre_parse` raises the magnitude `OverflowError` *before* it
        // checks the `min > max` relation, so a bound that is both oversized *and* inverted
        // (`a{99999999999,3}`) reports "the repetition number is too large", not "min repeat
        // greater than max repeat". The accept/reject verdict is identical either way (both
        // reject as `InvalidRegex`); the order makes the *reason* faithful to the oracle.
        if find_oversized_repeat_count(&pattern).is_some() {
            return Err(GrammarError::InvalidRegex {
                pattern: pattern.clone(),
                reason: "the repetition number is too large — a counted repetition \
                     `{m}`/`{m,}`/`{m,n}` has a bound at or above Python's MAXREPEAT \
                     (0xFFFFFFFF == 4294967295), e.g. `a{1,99999999999999999999}`. Python \
                     `re` rejects this with OverflowError (\"the repetition number is too \
                     large\"); lark-rs matches that rejection (ADR-0017)."
                    .to_string(),
            });
        }
        // **"Min repeat greater than max repeat" pre-screen (#534).** A counted repetition
        // `{m,n}` with `m > n` (`a{3,2}`) is a Python `re` "min repeat greater than max
        // repeat" build error, but the Rust `regex` crate *accepts* it (it compiles to an
        // empty/never-matching repeat), so `Regex::new` below would let the terminal build —
        // lark-rs would be *more permissive than the oracle* (the unfalsifiable direction,
        // ADR-0017). We classify the shape locally (`find_inverted_bound_quantifier`, class-
        // /escape-aware via the shared `RegexCursor`) so it surfaces as the truthful
        // `InvalidRegex`, never the misleading `LookaroundScope` refusal. Runs on the
        // normalized `pattern` (comments already stripped — an interior `(?#…)` that makes
        // Python read the braces as a literal is already `\{`-escaped — and `{,n}`/`{,}`
        // already `{0,n}`/`{0,}`, whose `0` lower is never an inverted bound). Both bounds
        // here are `< MAXREPEAT` — the #545 screen above already rejected any oversized one.
        if find_inverted_bound_quantifier(&pattern).is_some() {
            return Err(GrammarError::InvalidRegex {
                pattern: pattern.clone(),
                reason: "min repeat greater than max repeat — a counted repetition `{m,n}` \
                     has a lower bound greater than its upper bound (e.g. `a{3,2}`). Python \
                     `re` (sre_parse) rejects this as \"min repeat greater than max \
                     repeat\"; lark-rs matches that rejection (ADR-0017)."
                    .to_string(),
            });
        }
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

    /// #467: `Pattern` equality gates on the **variant first**, so a string literal is
    /// never equal to a regex of the same source — matching Python Lark's type-first
    /// `Pattern.__eq__` and the `patterns_equivalent` unification gate (#403/#440). Before
    /// the fix the `_ => as_regex_str() == as_regex_str()` arm reported
    /// `PatternStr("ab") == PatternRe(/ab/)` true (and the `as_regex_str().hash()` impl
    /// hashed them equal).
    ///
    /// This test asserts the real `Eq`/`Hash` contract — `a == b ⇒ hash(a) == hash(b)`,
    /// i.e. *equal* patterns hash equal — not the stronger (and uncontracted) claim that
    /// *unequal* patterns never collide. Rust's `Hash` makes no such no-collision promise
    /// (#528). Mixing the variant discriminant into the hash makes a collision between these
    /// two cross-kind patterns *practically* impossible under the default hasher, but that
    /// is a quality-of-implementation property, not a guarantee, so we do not assert it.
    #[test]
    fn pattern_eq_hash_gate_on_kind() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let h = |p: &Pattern| {
            let mut s = DefaultHasher::new();
            p.hash(&mut s);
            s.finish()
        };

        let str_ab = Pattern::Str(PatternStr::new("ab"));
        let re_ab = Pattern::Re(PatternRe::new("ab", 0).expect("regex builds"));

        // Cross-kind, same source: never equal (both directions). We deliberately do NOT
        // assert their hashes differ — that would over-claim a no-collision guarantee the
        // `Hash` contract does not make (see the doc comment above).
        assert_ne!(
            str_ab, re_ab,
            "PatternStr(\"ab\") must not equal PatternRe(/ab/)"
        );
        assert_ne!(re_ab, str_ab, "equality is symmetric across kinds");

        // The actual contract: equal patterns hash equal. Same-kind equality still holds
        // (and stays hash-consistent).
        let str_ab2 = Pattern::Str(PatternStr::new("ab"));
        let re_ab2 = Pattern::Re(PatternRe::new("ab", 0).expect("regex builds"));
        assert_eq!(str_ab, str_ab2);
        assert_eq!(h(&str_ab), h(&str_ab2));
        assert_eq!(re_ab, re_ab2);
        assert_eq!(h(&re_ab), h(&re_ab2));

        // `"ab"` and `"ab"i` are distinct string patterns (the pre-existing `ci` gate).
        let str_ab_ci = Pattern::Str(PatternStr::new_ci("ab"));
        assert_ne!(
            str_ab, str_ab_ci,
            "case-sensitivity distinguishes string patterns"
        );
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

    /// H6-2 (#400) + #447: `normalize_python_escapes` rewrites the Python-`re`
    /// empty-lower-bound quantifier `{,n}` (n ≥ 0, including the fully-empty `{,}`) →
    /// `{0,n}` / `{0,}` (Python reads `{,n}` as `{0,n}` and `{,}` as `{0,}` == `*`; the
    /// regex crate rejects the bare form). The rewrite is class-aware (a `{` inside `[...]`
    /// is a literal), escape-aware (a `\{` is a literal brace), and fires only on a
    /// `base_quantifier_len`-valid `{,…}` — a `{,x}` with a non-digit upper, an
    /// unterminated `{,3`, a bare `{}` (no comma), or a lower-bounded `{m,n}` is left
    /// byte-exact. The inverted bound `{3,2}` is left untouched by *normalization* (it has a
    /// lower bound, so it never matches this empty-lower-bound shape); it is rejected
    /// separately as "min repeat greater than max repeat" (#534, see
    /// `inverted_bound_quantifier_rejected`), not here.
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
        // Inverted bound — NOT this empty-lower-bound shape; left byte-exact by
        // *normalization* (it is rejected separately as "min repeat greater than max repeat",
        // #534 — see `inverted_bound_quantifier_rejected`).
        assert_eq!(normalize_python_escapes("a{3,2}b"), "a{3,2}b");
        // Class-aware: a `{,3}` *inside* a character class is a set of literal chars in
        // Python (`{`, `,`, `3`, `}`), not a quantifier — left untouched.
        assert_eq!(normalize_python_escapes("[a{,3}]"), "[a{,3}]");
        // Escape-aware: a `\{` is a literal brace, never a quantifier open.
        assert_eq!(normalize_python_escapes("a\\{,3}"), "a\\{,3}");
        // A non-digit upper (`{,x}`) / unterminated (`{,3`) is a literal brace run in
        // Python — NOT this empty-lower-bound shape, so it is not rewritten to `{0,…}`.
        // Since #462 the literal `{` is escaped (`{` → `\{`) so the regex crate reads it as
        // a literal brace (it otherwise rejects a non-numeric brace body), matching Python's
        // literal interpretation — distinct from the `{0,n}` quantifier rewrite above.
        assert_eq!(normalize_python_escapes("a{,x}b"), "a\\{,x}b"); // non-digit upper → literal `\{`
        assert_eq!(normalize_python_escapes("a{,3"), "a\\{,3"); // unterminated (no `}`) → literal `\{`
                                                                // The fully-empty `{,}` is Python's `{0,}` (== `*`), NOT a literal brace run
                                                                // (#447): it is now recognized and rewritten exactly like `{,n}` (zero upper
                                                                // digits). `re.match(r'a{,}b','aaab')` matches, so `/a{,}b/` must build as `a*b`.
        assert_eq!(normalize_python_escapes("a{,}b"), "a{0,}b");
        assert_eq!(normalize_python_escapes("{,}"), "{0,}");
        // Class-aware / escape-aware for `{,}` too: a `{,}` inside `[...]` or after `\`
        // is a literal brace run in Python, never a quantifier — left untouched.
        assert_eq!(normalize_python_escapes("[a{,}]"), "[a{,}]");
        assert_eq!(normalize_python_escapes("a\\{,}"), "a\\{,}");
        // The bare `{}` (no comma, no digit) is a literal brace pair — the `{0,n}` widening
        // keys on "digit *or* comma", and `{}` has neither. Since #462 it too is escaped
        // (`{` → `\{`) so the regex crate reads the literal brace, matching Python.
        assert_eq!(normalize_python_escapes("a{}b"), "a\\{}b");
        // #462 literal brace runs: a `{` whose body is not a valid quantifier bound is
        // escaped to a literal `\{` (a bare `}` is already a literal to the crate). Class-
        // and escape-aware: a `{` inside `[...]` or after `\` is left as-is.
        assert_eq!(normalize_python_escapes("a{x}b"), "a\\{x}b");
        assert_eq!(normalize_python_escapes("{x}"), "\\{x}");
        assert_eq!(normalize_python_escapes("N{x}"), "N\\{x}");
        assert_eq!(normalize_python_escapes("a{"), "a\\{"); // lone unterminated brace
        assert_eq!(normalize_python_escapes("{ 2}"), "\\{ 2}"); // space → not a quantifier
        assert_eq!(normalize_python_escapes("{a,b}"), "\\{a,b}"); // non-digit bound
                                                                  // A real quantifier is never escaped (the literal-brace branch keys on
                                                                  // `base_quantifier_len`, the shared quantifier oracle).
        assert_eq!(normalize_python_escapes("a{2}b"), "a{2}b");
        assert_eq!(normalize_python_escapes("a{2,3}b"), "a{2,3}b");
        // Mixed: a real `{2}` is kept, a sibling literal `{x}` is escaped.
        assert_eq!(normalize_python_escapes("a{2}{x}"), "a{2}\\{x}");
        // Class-aware / escape-aware for a literal `{x}` too.
        assert_eq!(normalize_python_escapes("[a{x}]"), "[a{x}]"); // in-class `{` is literal already
        assert_eq!(normalize_python_escapes("a\\{x}"), "a\\{x}"); // already-escaped `\{` untouched
    }

    /// #509: a `(?#…)` comment that falls **inside** a `{…}` brace run makes the whole
    /// brace run a *literal* in Python `re` — Python's quantifier-bound scan only accepts
    /// `digits`/`,`/`}`, so a comment-open `(?#` between `{` and `}` aborts the quantifier
    /// read and `re.compile('{3(?#c)}').fullmatch('{3}')` matches the literal text `{3}`.
    /// The comment-strip in `normalize_python_escapes` must therefore leave the brace run
    /// literal (escape the `{` to `\{`), not strip the comment first and then re-recognize
    /// the leftover `{3}` as a real quantifier (which both the regex crate and the local
    /// "nothing to repeat" pre-screen then reject — an over-rejection of input Python
    /// accepts). Oracle: Python `re` (verified 3.11). A comment *outside* the braces
    /// (before `{` or after `}`) leaves the real quantifier intact.
    #[test]
    fn normalize_comment_inside_braces_is_literal() {
        // The issue repro: `{3(?#c)}` → literal `\{3}` (matches the text `{3}`), NOT `{3}`.
        assert_eq!(normalize_python_escapes("{3(?#c)}"), "\\{3}");
        assert_eq!(normalize_python_escapes("a{3(?#c)}"), "a\\{3}");
        // Comment anywhere inside the brace span aborts the quantifier read → literal.
        assert_eq!(normalize_python_escapes("{(?#c)3}"), "\\{3}"); // before the digit
        assert_eq!(normalize_python_escapes("{1(?#c)2}"), "\\{12}"); // between digits
        assert_eq!(normalize_python_escapes("{3,(?#c)5}"), "\\{3,5}"); // around the comma
        assert_eq!(normalize_python_escapes("{3,5(?#c)}"), "\\{3,5}"); // after the upper bound
        assert_eq!(normalize_python_escapes("{,(?#c)3}"), "\\{,3}"); // empty-lower-bound form
        assert_eq!(normalize_python_escapes("{(?#c)x}"), "\\{x}"); // already-literal body + comment
                                                                   // A second brace run with an interior comment beside a real quantifier: only the
                                                                   // comment-bearing run goes literal; the real `{2}` is untouched.
        assert_eq!(normalize_python_escapes("a{2}{3(?#c)}"), "a{2}\\{3}");
        // Class-aware: inside `[...]` a `{` is already a literal and the `(?#` is literal
        // class text (not a comment) — left byte-exact.
        assert_eq!(normalize_python_escapes("[{3(?#c)}]"), "[{3(?#c)}]");
        // Escape-aware: a `\{` is already a literal brace; the comment after it still strips
        // (it is outside any brace *quantifier* run — there is no quantifier to abort).
        assert_eq!(normalize_python_escapes("\\{3(?#c)}"), "\\{3}");

        // CONTROL — a comment OUTSIDE the braces leaves the real quantifier intact.
        // `a{3}(?#c)` and `a(?#c){3}` both compile in Python as the quantifier `a{3}`.
        assert_eq!(normalize_python_escapes("a{3}(?#c)"), "a{3}");
        assert_eq!(normalize_python_escapes("a(?#c){3}"), "a{3}");
        assert_eq!(normalize_python_escapes("a{3}(?#c)b"), "a{3}b");

        // The normalized literal brace carries no "nothing to repeat" shape (it is a
        // literal `\{`, not a leading `{3}` quantifier), so the #448/#506 pre-screen is
        // clean — the regression the issue describes (`{3(?#c)}` → `{3}` → rejected) is
        // gone at every stage.
        assert!(find_nothing_to_repeat(&normalize_python_escapes("{3(?#c)}")).is_none());
        assert!(find_nothing_to_repeat(&normalize_python_escapes("{3,5(?#c)}")).is_none());
        assert!(find_nothing_to_repeat(&normalize_python_escapes("{,(?#c)3}")).is_none());
    }

    /// #509 end-to-end: a `/…/` terminal whose only "quantifier" is a `{…}` brace run with
    /// an interior `(?#…)` comment **builds** and **matches the literal braces**, exactly
    /// as Python `re` does (`re.compile('{3(?#c)}').fullmatch('{3}')` matches). Before the
    /// fix the stripped comment left `{3}`, which the `regex` crate / nothing-to-repeat
    /// pre-screen rejected — an over-rejection of input Python accepts. We assert the build
    /// succeeds and the compiled regex matches `{3}` (and does NOT match three repeats —
    /// it is literal, not a quantifier). Oracle: Python `re` 3.11.
    #[test]
    fn comment_inside_braces_builds_and_matches_literal() {
        for (pat, lit, three_repeats) in [
            ("{3(?#c)}", "{3}", "xxx"),
            ("a{3(?#c)}", "a{3}", "aaa"),
            ("{3,5(?#c)}", "{3,5}", "xxxxx"),
            ("{,(?#c)3}", "{,3}", "xxx"),
        ] {
            let p = PatternRe::new(pat, 0)
                .unwrap_or_else(|e| panic!("/{pat}/ should build (Python accepts it): {e:?}"));
            // Anchor a full match against the *literal* text (the braces are literal).
            let rx = Regex::new(&format!("^(?:{})$", p.pattern))
                .unwrap_or_else(|e| panic!("compiled /{pat}/ → {:?} invalid: {e:?}", p.pattern));
            assert!(
                rx.is_match(lit),
                "/{pat}/ (→ {:?}) must match the literal {lit:?} (Python oracle)",
                p.pattern
            );
            assert!(
                !rx.is_match(three_repeats),
                "/{pat}/ (→ {:?}) is a literal brace run, not a quantifier — must NOT match \
                 the repeated-char string {three_repeats:?}",
                p.pattern
            );
        }
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
        // #447: `{,}` is now a base quantifier (Python's `{0,}`), so two adjacent `{,}`
        // (or a `{,}` stacked on another quantifier) is a multiple repeat Python rejects.
        for p in ["a{,}{,}", "a{,}{,}b", "a{,}*", "a*{,}", "a{,}{2}"] {
            assert!(
                reject_quantifier_dialect_divergence(p).is_err(),
                "{p:?} stacks `{{,}}` (== `{{0,}}`) on a quantifier — must be refused as a \
                 multiple repeat"
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
            "a{,}",   // #447: a single `{,}` (== `{0,}`) is a valid quantifier, not stacked
            "a{,}b",  // #447: `{,}` followed by a literal is fine (one repeat on `a`)
            "[a{,}]", // #447: `{,}` inside a class is literal — never a quantifier
            "a\\{,}", // #447: `\{,}` is a literal brace run
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

    /// #448: a leading/dangling quantifier with **nothing to repeat** (`*a`, `+a`, `?a`,
    /// `{0,3}`, the post-normalization `{,3}`/`{,}` of #400/#447, `(?#c)*a` after a
    /// stripped zero-width comment, `(?:*a)`) is a Python `re` "nothing to repeat" build
    /// error — a malformed quantifier, *not* lookaround. `PatternRe::new` must reject each
    /// with the corrected `InvalidRegex` "nothing to repeat" category, NOT the misleading
    /// `LookaroundScope`/`OutOfScope` "backtracking-only syntax" message it emitted before
    /// the fix. The reject decision itself is unchanged (parity with Python preserved) —
    /// this is an error-taxonomy correction only.
    #[test]
    fn nothing_to_repeat_is_invalid_regex_not_lookaround_scope() {
        for p in [
            "*a", "+a", "?a", "{0,3}", "{,3}", "{,}", "(?#c)*a", "(?:*a)",
        ] {
            let err =
                PatternRe::new(p, 0).expect_err("a nothing-to-repeat quantifier must still reject");
            match &err {
                GrammarError::InvalidRegex { reason, .. } => assert!(
                    reason.contains("nothing to repeat"),
                    "{p:?}: InvalidRegex reason must name \"nothing to repeat\", got: {reason}"
                ),
                other => panic!(
                    "{p:?}: must be InvalidRegex \"nothing to repeat\", not {other:?} \
                     (a LookaroundScope here is the #448 mis-categorization)"
                ),
            }
        }
    }

    /// #510: a base quantifier applied to a **bare zero-width anchor** — an out-of-class
    /// `^`/`$` or an anchor escape `\b`/`\B`/`\A`/`\Z` — is Python `re` "nothing to repeat",
    /// but the `regex` crate accepts it (it lets you quantify an anchor), so `Regex::new`
    /// would build a terminal Python rejects: lark-rs was *more permissive* than the oracle
    /// (ADR-0017's unfalsifiable corollary). `PatternRe::new` must reject each with the
    /// truthful `InvalidRegex` "nothing to repeat" category, matching Python. Grounded
    /// against Python 3.11 `re.compile` (the worker's differential probe; see issue #510).
    /// Scope is strictly the quantifier-binding question — whether lark-rs *supports*
    /// `\b`/`\Z` semantics at all is the parked anchor-policy fork (#275), untouched here.
    #[test]
    fn quantified_bare_anchor_is_nothing_to_repeat() {
        for p in [
            "^*", "^+", "^?", "^{2,5}", "$*", "$+", "$?", r"\b*", r"\B*", r"\A*", r"\Z*", r"\b+",
            r"\Z{2}", "(?m)^*", "(?m)$*", r"(?m)\b*", r"a\b*", "^$*",
        ] {
            let err = PatternRe::new(p, 0)
                .expect_err("a quantified bare anchor is nothing to repeat (Python `re`)");
            match &err {
                GrammarError::InvalidRegex { reason, .. } => assert!(
                    reason.contains("nothing to repeat"),
                    "{p:?}: InvalidRegex reason must name \"nothing to repeat\", got: {reason}"
                ),
                other => panic!("{p:?}: must be InvalidRegex \"nothing to repeat\", not {other:?}"),
            }
        }
    }

    /// #510 negative control: an anchor followed by a **real** atom that the quantifier binds
    /// to — or an anchor inside a character class where `^`/`$`/`\b` are literal members —
    /// still builds. This is the boundary against the parked #275 anchor-support policy: an
    /// *un*-quantified `\bword\b` (and `a$`, `^a*`, …) must build exactly as before; the fix
    /// only flags the quantifier-on-anchor shape, never anchor support itself. Each builds in
    /// Python `re` (oracle).
    #[test]
    fn anchor_then_real_atom_still_builds() {
        for p in [
            "^a*",
            "a$",
            "^(a)*",
            r"\bword\b",
            "a*$",
            r"(\b)*",
            r"\Bx*",
            r"[\^]*",
            "[$]*",
            r"[\b]*",
            "[^a]*",
        ] {
            assert!(
                PatternRe::new(p, 0).is_ok(),
                "{p:?}: the quantifier binds to a real atom (or the anchor is a class \
                 member) — Python `re` accepts it, so lark-rs must build it (#510, not #275)"
            );
        }
    }

    /// #448 differential-audit: the nothing-to-repeat pre-screen (#506: now a **local**
    /// shape classifier, `find_nothing_to_repeat`, no longer the `regex`-crate message text)
    /// must NOT re-bucket a genuine `LookaroundScope`/backref input. These constructs
    /// (zero-width lookahead, the general-internal-lookahead `(?:…)` form, a backreference)
    /// are routed to the lookaround analyzer / refusal seam and must stay there (or build,
    /// for a lowerable lookbehind). `PatternRe::new` itself does NOT gate lowerable
    /// lookaround — it constructs cleanly and the verdict is deferred to the lexer-build
    /// routing — so the assertion here is only that these do NOT come back as an
    /// `InvalidRegex` "nothing to repeat" (the re-bucketing failure mode).
    #[test]
    fn real_lookaround_not_rebucketed_as_nothing_to_repeat() {
        for p in ["(?=ab)", "(?<=ab)c", "a(?=b)", r"(a)\1", r"(?P<x>a)(?P=x)"] {
            if let Err(GrammarError::InvalidRegex { reason, .. }) = PatternRe::new(p, 0) {
                assert!(
                    !reason.contains("nothing to repeat"),
                    "{p:?}: a real lookaround/backref input must NOT be re-bucketed as a \
                     nothing-to-repeat malformed quantifier (#448 re-bucketing regression)"
                );
            }
        }
    }

    /// #506 unit-level differential: `find_nothing_to_repeat` classifies the shape *locally*
    /// (class-/escape-aware via the shared `RegexCursor`) and must agree with Python `re`'s
    /// "nothing to repeat" verdict over an adversarial corpus — WITHOUT leaning on the
    /// `regex` crate's diagnostic text. The screen runs on the **normalized** pattern, so the
    /// inputs here are written post-normalization (no `(?#…)` comment, `{0,n}` not `{,n}`).
    /// Each `true` case is one Python `re` flags "nothing to repeat at position …"; each
    /// `false` case is one Python `re` accepts (or rejects for a *different* reason — e.g.
    /// "multiple repeat", which is a distinct screen). Grounded against Python 3.11
    /// (`re.compile`) by the worker's differential probe.
    #[test]
    fn find_nothing_to_repeat_matches_python_shapes() {
        // (pattern, is_nothing_to_repeat) — pattern is already normalized.
        let cases: &[(&str, bool)] = &[
            // Leading quantifiers — nothing precedes.
            ("*a", true),
            ("+a", true),
            ("?a", true),
            ("{0,3}", true),
            ("{0,}", true),
            ("{3}", true),
            ("{3,5}", true),
            // After a group/assertion *open* — body has nothing yet.
            ("(*a)", true),
            ("(?:*a)", true),
            ("(?=*a)", true),
            ("(?!*a)", true),
            ("(?<=*a)", true),
            ("(?<!*a)", true),
            ("(?P<n>*a)", true),
            ("(?i:*a)", true),
            // After an alternation bar — the arm has nothing yet.
            ("(|*a)", true),
            ("a|*b", true),
            ("a|+b", true),
            ("(a|?b)", true),
            // After a bodiless inline flag group — consumes nothing repeatable.
            ("(?i)*a", true),
            // --- negative controls: Python accepts, or rejects for a different reason ---
            // Quantifier has a real preceding atom.
            ("a*", false),
            ("a+", false),
            ("a?", false),
            ("a{0,3}", false),
            ("(a)*", false),
            ("(?:a)*", false),
            ("a|b*", false),
            // A *closed* group/assertion is itself repeatable.
            ("()*", false),
            ("(?:)*", false),
            ("(a|)*", false),
            ("(?=a)*b", false),
            ("(?<=a)*b", false),
            // Inside a character class a quantifier char is a literal member.
            ("[*]", false),
            ("[+]", false),
            ("[?]", false),
            ("[{0,3}]", false),
            // An escaped quantifier is a literal.
            (r"\*a", false),
            (r"\+a", false),
            (r"a\?", false),
            // A named backreference is a repeatable atom.
            (r"(?P<n>a)(?P=n)*x", false),
            // Real lookaround/backref carry no nothing-to-repeat shape.
            ("(?=ab)", false),
            ("(?<=ab)c", false),
            ("a(?=b)", false),
            (r"(a)\1", false),
            // A second quantifier on a quantifier is "multiple repeat" (a different screen),
            // NOT "nothing to repeat".
            ("a**", false),
            ("a*?", false),
            // --- #510: a quantifier on a bare zero-width anchor is "nothing to repeat" ---
            // Out-of-class `^`/`$` are anchors, not repeatable atoms.
            ("^*", true),
            ("^+", true),
            ("^?", true),
            ("^{0,5}", true),
            ("$*", true),
            ("$+", true),
            ("$?", true),
            // The anchor escapes `\b`/`\B`/`\A`/`\Z` likewise have nothing to repeat.
            (r"\b*", true),
            (r"\B*", true),
            (r"\A*", true),
            (r"\Z*", true),
            (r"\b+", true),
            (r"\Z{0,2}", true),
            // A flag prefix does not change the verdict (the anchor still has nothing).
            ("(?m)^*", true),
            (r"(?m)\b*", true),
            // An anchor with nothing real before it still poisons a following quantifier:
            // the `*` binds to the anchor, not the earlier atom (Python: `a\b*` rejects).
            (r"a\b*", true),
            ("^$*", true),
            ("$^*", true),
            // --- #510 negative controls: the quantifier binds to a REAL atom, builds ---
            ("^a*", false),   // `*` binds to `a`
            ("a$", false),    // `$` is a trailing anchor, no quantifier
            ("^(a)*", false), // `*` binds to the closed group
            (r"\bword\b", false),
            ("a*$", false),
            ("(\\b)*", false), // `*` binds to the closed group, not the inner `\b`
            (r"\Bx*", false),  // `*` binds to `x`
            // Inside a class `^`/`$` are literal members and `\b` is a backspace literal —
            // all repeatable, so a following quantifier has something to repeat.
            (r"[\^]*", false),
            ("[$]*", false),
            (r"[\b]*", false),
            ("[^a]*", false), // leading `^` is class negation; the class is repeatable
        ];
        for &(p, want) in cases {
            assert_eq!(
                find_nothing_to_repeat(p).is_some(),
                want,
                "{p:?}: find_nothing_to_repeat should be {want} (Python `re` oracle)"
            );
        }
    }

    /// #506: the nothing-to-repeat classification is independent of the `regex` crate's
    /// error-message *text*. A leading quantifier still rejects with the truthful
    /// `InvalidRegex` "nothing to repeat" category even though the screen never reads
    /// `e.to_string()`. (The #448 pins above assert the category for the end-to-end
    /// `PatternRe::new` path; this pins that the *local* screen, not the crate message, is
    /// what produced the verdict — `find_nothing_to_repeat` fires on the normalized pattern.)
    #[test]
    fn nothing_to_repeat_is_local_not_message_text() {
        // A construct whose Python-`re` rejection happens to also be "nothing to repeat"
        // after the `{,}`→`{0,}` normalization (#447) — the local screen must see it on the
        // normalized form, with no dependence on any crate diagnostic string.
        assert!(find_nothing_to_repeat(&normalize_python_escapes("{,}a")).is_some());
        assert!(find_nothing_to_repeat(&normalize_python_escapes("(?#c)*a")).is_some());
        // And the end-to-end category is the truthful one.
        let err = PatternRe::new("{,}a", 0).expect_err("leading {,} is nothing to repeat");
        match err {
            GrammarError::InvalidRegex { reason, .. } => {
                assert!(reason.contains("nothing to repeat"), "got: {reason}")
            }
            other => panic!("expected InvalidRegex nothing-to-repeat, got {other:?}"),
        }
    }

    /// #506 audit-pin: a *doubled* leading quantifier (`**`, `(?i:**`, `|**`, …) is rejected
    /// by both engines, but Python `re` reports it as "nothing to repeat" (the first `*` has
    /// nothing to repeat) whereas lark-rs reports "multiple repeat" — because
    /// `reject_quantifier_dialect_divergence` runs on the **raw** source *before* this
    /// nothing-to-repeat screen and catches the doubled quantifier first. This is a
    /// **pre-existing** screen-ordering artifact (the prior message-string check was likewise
    /// shadowed by that earlier screen), it is purely a divergence between two adjacent
    /// *reject* categories, and accept/reject parity with Python is preserved. The worker's
    /// 580-body differential audit (#506) flagged exactly this family (33 bodies, all the
    /// `**`-with-nothing-before shape) and *no* accept/reject divergence. Pinned so the
    /// behavior is intentional, not silent — full category parity here is tracked separately
    /// if it ever matters.
    #[test]
    fn doubled_leading_quantifier_rejects_as_multiple_repeat_audit_pin() {
        for p in ["**", "(?i:**)", "|**", "(?P<n>**)"] {
            match PatternRe::new(p, 0) {
                Err(GrammarError::InvalidRegex { reason, .. }) => assert!(
                    reason.contains("multiple repeat"),
                    "{p:?}: expected the earlier multiple-repeat screen to catch the doubled \
                     quantifier first, got: {reason}"
                ),
                other => panic!("{p:?}: must reject (both engines do), got {other:?}"),
            }
        }
    }

    /// #534 unit-level differential: `find_inverted_bound_quantifier` classifies the
    /// "min repeat greater than max repeat" shape *locally* (class-/escape-aware via the
    /// shared `RegexCursor`) and must agree with Python `re`'s verdict — a counted
    /// repetition `{m,n}` is inverted iff both bounds are present and `m > n`. The screen
    /// runs on the **normalized** pattern, so inputs here are written post-normalization (no
    /// `(?#…)` comment; `{,n}`/`{,}` already `{0,n}`/`{0,}`). Grounded against Python 3.11.
    #[test]
    fn find_inverted_bound_quantifier_matches_python_shapes() {
        // (normalized pattern, is_inverted) — `true` ⇒ Python "min repeat greater than max".
        let cases: &[(&str, bool)] = &[
            // Inverted: both bounds present, m > n.
            ("a{3,2}", true),
            ("a{3,2}b", true),
            ("{3,2}", true),
            ("a{10,2}", true),   // multi-digit lower
            ("a{100,50}", true), // multi-digit both
            ("a{99,1}", true),
            // Bounds too large for any integer type — the min>max relation is still decided
            // by the digit-slice compare (Python raises on the count too; both reject).
            ("a{99999999999999999999,1}", true),
            ("a{007,5}", true), // leading zeros: 7 > 5
            // Equal value with leading zeros is NOT inverted (`007` == `7`).
            ("a{007,7}", false),
            ("(a){3,2}", true), // quantifier on a closed group
            ("[a-z]{3,2}", true),
            ("a{2,3}b{3,2}", true), // a later one inverted
            // --- negatives: Python accepts (or rejects for a different reason) ---
            ("a{2,3}", false), // m < n
            ("a{2,2}", false), // m == n
            ("a{0,0}", false),
            ("a{2,}", false), // open upper — not the both-bounds form
            ("a{1,}", false),
            ("a{3}", false),    // single bound
            ("a{0,3}", false),  // the post-normalization `{,3}` — `0` lower, never inverted
            ("a{0,}", false),   // the post-normalization `{,}`
            ("a{2,10}", false), // lexicographically `2` < `10` but numerically too — must NOT
            // be a string compare
            // Class-aware: a `{3,2}` inside `[...]` is a set of literal chars, not a
            // quantifier.
            ("[a{3,2}]", false),
            // Escape-aware: a `\{` is a literal brace (this is what the comment-bearing
            // `a{3(?#c),2}` normalizes to), never a quantifier.
            (r"a\{3,2}", false),
            // A literal-brace body is not a quantifier at all.
            ("a{x,y}", false),
            ("a{3,2", false), // unterminated — not a well-formed quantifier
        ];
        for &(p, want) in cases {
            assert_eq!(
                find_inverted_bound_quantifier(p).is_some(),
                want,
                "{p:?}: find_inverted_bound_quantifier should be {want} (Python `re` oracle)"
            );
        }
    }

    /// #534 end-to-end: a `/…/` terminal containing an inverted-bound `{m,n}` (m > n) is
    /// **rejected** at build with a correctly-categorized `InvalidRegex` carrying the
    /// "min repeat greater than max repeat" reason (NOT a `LookaroundScope` refusal),
    /// matching Python `re` (`re.compile('a{3,2}')` → "min repeat greater than max repeat").
    /// The negative controls — non-inverted `{m,n}`, equal bounds, open `{m,}`, the
    /// post-normalization `{,n}`/`{,}`, an in-class `{3,2}`, and an escaped `\{3,2}` — all
    /// still **build** (Python accepts each). Oracle: Python `re` 3.11.
    #[test]
    fn inverted_bound_quantifier_rejected() {
        // Rejected — both engines disagree (regex crate accepts, Python rejects); lark-rs
        // matches Python.
        for p in [
            "a{3,2}",
            "a{3,2}b",
            "a{10,2}",
            "a{100,50}",
            "(a){3,2}",
            "[a-z]{3,2}",
        ] {
            match PatternRe::new(p, 0) {
                Err(GrammarError::InvalidRegex { reason, .. }) => assert!(
                    reason.contains("min repeat greater than max repeat"),
                    "/{p}/: expected the truthful min>max InvalidRegex, got: {reason}"
                ),
                other => panic!(
                    "/{p}/: an inverted-bound quantifier must be rejected (Python `re` does), \
                     got {other:?}"
                ),
            }
        }
        // Accepted — Python compiles each, so lark-rs must build them.
        for p in [
            "a{2,3}", "a{2,2}", "a{0,0}", "a{2,}", "a{1,}", "a{3}", "a{2,10}", "a{,3}", "a{,}",
            "[a{3,2}]", r"a\{3,2}",
        ] {
            assert!(
                PatternRe::new(p, 0).is_ok(),
                "/{p}/: Python `re` accepts it — lark-rs must build it (#534 must not \
                 over-reject)"
            );
        }
    }

    /// #545 unit: `find_oversized_repeat_count` flags a `{m}`/`{m,}`/`{m,n}` whose lower
    /// **or** upper bound is `>= 0xFFFFFFFF` (Python's `sre_constants.MAXREPEAT`), the
    /// smallest count Python `re` rejects ("the repetition number is too large").
    /// `0xFFFFFFFE` and below build in both engines. Bounds too large for any integer type
    /// are compared by the same overflow-safe digit-slice magnitude check, so a 20-digit
    /// count is still flagged. Class- and escape-aware via the shared `RegexCursor`. Runs on
    /// the normalized pattern (`{,n}`/`{,}` already `{0,n}`/`{0,}`). Grounded against
    /// Python 3.11 (`sre_constants.MAXREPEAT == 0xFFFFFFFF`).
    #[test]
    fn find_oversized_repeat_count_matches_python_threshold() {
        // (normalized pattern, is_oversized) — `true` ⇒ Python "the repetition number is
        // too large".
        let cases: &[(&str, bool)] = &[
            // At/over MAXREPEAT (0xFFFFFFFF == 4294967295) — Python rejects.
            ("a{4294967295}", true),             // == MAXREPEAT, exact form
            ("a{4294967296}", true),             // just over (also over the regex-crate u32 cap)
            ("a{1,4294967295}", true),           // upper == MAXREPEAT
            ("a{4294967295,}", true),            // open upper, lower == MAXREPEAT
            ("a{4294967295,9999999999}", true),  // both over (lower triggers)
            ("a{1,99999999999999999999}", true), // 20-digit upper, way over
            ("a{99999999999999999999}", true),   // 20-digit exact
            ("a{99999999999999999999,}", true),  // 20-digit open lower
            ("a{0042949672950}", true),          // leading zeros: significant tail == MAXREPEAT
            ("(a){4294967296}", true),           // quantifier on a closed group
            ("[a-z]{4294967296}", true),         // on a class
            ("a{2,3}b{4294967296}", true),       // a later one oversized
            // --- negatives: Python accepts (count fits) ---
            ("a{4294967294}", false), // == MAXREPEAT-1, the largest accepted
            ("a{1,4294967294}", false), // upper at the cap-1
            ("a{4294967294,}", false),
            ("a{1000,2000}", false),
            ("a{3}", false),
            ("a{2,3}", false),
            ("a{0,}", false), // the post-normalization `{,}`
            ("a{0,3}", false),
            ("a{00000000005}", false), // many leading zeros, small value
            // Class-aware: an in-class brace run is literal members, not a quantifier.
            ("[a{99999999999999999999}]", false),
            // Escape-aware: a `\{` is a literal brace.
            (r"a\{99999999999999999999}", false),
            // Not a well-formed quantifier — a literal brace run, never sized.
            ("a{x}", false),
            ("a{99999999999999999999", false), // unterminated
        ];
        for &(p, want) in cases {
            assert_eq!(
                find_oversized_repeat_count(p).is_some(),
                want,
                "{p:?}: find_oversized_repeat_count should be {want} (Python `re` oracle, \
                 MAXREPEAT == 0xFFFFFFFF)"
            );
        }
    }

    /// #545 end-to-end: a `/…/` terminal with a counted-repeat bound `>= 0xFFFFFFFF` is
    /// **rejected** at build with a correctly-categorized `InvalidRegex` carrying the
    /// "repetition number is too large" reason (NOT a `LookaroundScope` refusal), matching
    /// Python `re` (`re.compile('a{1,99999999999999999999}')` → `OverflowError: the
    /// repetition number is too large`). The negative controls — a count at MAXREPEAT-1, a
    /// small `{m,n}`, an in-class brace run, an escaped `\{` — all still **build** (Python
    /// accepts each). Oracle: Python `re` 3.11.
    #[test]
    fn oversized_repeat_count_rejected() {
        // Rejected — both engines disagree (regex crate / lookaround analyzer accept up to
        // their own caps, Python rejects at MAXREPEAT); lark-rs matches Python.
        for p in [
            "a{1,99999999999999999999}", // the issue's repro
            "a{99999999999999999999}",
            "a{99999999999999999999,}",
            "a{4294967295}",   // == MAXREPEAT, exact
            "a{1,4294967295}", // == MAXREPEAT, upper
            "a{4294967295,}",  // == MAXREPEAT, open lower
            "a{4294967296}",   // just over (over the regex-crate cap too)
        ] {
            match PatternRe::new(p, 0) {
                Err(GrammarError::InvalidRegex { reason, .. }) => assert!(
                    reason.contains("repetition number is too large"),
                    "/{p}/: expected the truthful too-large InvalidRegex, got: {reason}"
                ),
                other => panic!(
                    "/{p}/: an over-large repeat count must be rejected (Python `re` raises \
                     OverflowError), got {other:?}"
                ),
            }
        }
        // Accepted — Python compiles each (count fits under MAXREPEAT), so lark-rs must
        // build them.
        for p in [
            "a{4294967294}", // == MAXREPEAT-1, the largest accepted
            "a{1,4294967294}",
            "a{4294967294,}",
            "a{1000,2000}",
            "a{3}",
            "a{2,3}",
            "[a{99999999999999999999}]", // in-class literal
            r"a\{99999999999999999999}", // escaped literal brace
        ] {
            assert!(
                PatternRe::new(p, 0).is_ok(),
                "/{p}/: Python `re` accepts it (count fits) — lark-rs must build it (#545 \
                 must not over-reject)"
            );
        }
        // A bound that is **both** oversized *and* inverted (`a{99999999999,3}`) must report
        // the *magnitude* reason, not the inverted one — Python's `sre_parse` raises the count
        // `OverflowError` *before* it checks `min > max` (verified: `a{99999999999,3}`,
        // `a{3,99999999999}`, `a{99999999999,1}` all → "the repetition number is too large").
        // The #545 screen is ordered before the #534 screen to match.
        match PatternRe::new("a{99999999999,3}", 0) {
            Err(GrammarError::InvalidRegex { reason, .. }) => assert!(
                reason.contains("repetition number is too large"),
                "oversized+inverted must report the magnitude reason first (Python order), \
                 got: {reason}"
            ),
            other => panic!("a{{99999999999,3}} must be rejected, got {other:?}"),
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

    /// #364 corrective: `strip_screening_comments` removes the comment spans the semantic
    /// dialect screens must not see, mirroring Python `re`'s comment removal — `(?#…)`
    /// always, `# …`-to-EOL only under VERBOSE — both **outside a class** and
    /// **escape-aware**, while preserving whitespace and class bodies verbatim.
    #[test]
    fn strip_screening_comments_removes_only_comments() {
        use flags::VERBOSE;
        // `(?#…)` is stripped regardless of flags; the span ends at the first unescaped
        // `)` (the shared `end_of_inline_comment` rule), so `a(?#(?<x>)b` → `ab`.
        assert_eq!(strip_screening_comments("a(?#c)b", 0), "ab");
        assert_eq!(strip_screening_comments(r"a(?#(?<x>)b", 0), "ab");
        assert_eq!(strip_screening_comments(r"a(?#\N{BULLET})b", 0), "ab");
        // `\)` inside the comment body does not end it.
        assert_eq!(strip_screening_comments(r"a(?#x\)y)b", 0), "ab");
        // An unterminated `(?#…` swallows the rest (Python build-errors on it; the
        // quantifier screen on `raw` is what reports that — here we just don't choke).
        assert_eq!(strip_screening_comments("a(?#noend", 0), "a");
        // Inside a character class, `(?#` and `#` are literal members — NOT a comment.
        assert_eq!(strip_screening_comments("[a(?#)]z", 0), "[a(?#)]z");
        assert_eq!(strip_screening_comments("[#(?<x>]z", VERBOSE), "[#(?<x>]z");
        // An escaped `\(` is not a comment open; an escaped `\#` is a literal, not a
        // verbose comment — both copied verbatim (escape pair preserved).
        assert_eq!(strip_screening_comments(r"a\(?#c)b", 0), r"a\(?#c)b");
        assert_eq!(strip_screening_comments(r"a\#b", VERBOSE), r"a\#b");
        // Verbose `# …` to end-of-line is stripped ONLY under VERBOSE; the newline is kept.
        assert_eq!(
            strip_screening_comments("a # cmt (?<x>\nb", VERBOSE),
            "a \nb"
        );
        // …and is a LITERAL `#` (kept) when VERBOSE is off.
        assert_eq!(strip_screening_comments("a # cmt\nb", 0), "a # cmt\nb");
        // Whitespace is preserved verbatim (NOT collapsed): Python does not fuse
        // whitespace-separated tokens into a group under VERBOSE.
        assert_eq!(strip_screening_comments("a   b", VERBOSE), "a   b");
        assert_eq!(strip_screening_comments("( ?<x>)", VERBOSE), "( ?<x>)");
        // A real `(?#…)` comment is still stripped even under VERBOSE.
        assert_eq!(strip_screening_comments("a (?#c) b", VERBOSE), "a  b");

        // ── Scoped inline verbose `(?x:…)` (the composite-terminal bake path, flags == 0).
        // The `#` comment inside the wrapper is verbose-stripped even though the bitset is 0;
        // the wrapper syntax itself is copied through.
        assert_eq!(
            strip_screening_comments("(?x:a # cmt (?<x>\nb)", 0),
            "(?x:a \nb)"
        );
        // `(?x)` bodiless turns verbose on for the remainder of its scope.
        assert_eq!(
            strip_screening_comments("(?x)a # c (?<x>\nb", 0),
            "(?x)a \nb"
        );
        // `(?-x:…)` nested inside `(?x:…)` turns verbose OFF in its scope — the `#` there is
        // a literal again; outside that inner scope verbose is still on.
        assert_eq!(
            strip_screening_comments("(?x:a (?-x: #lit )b # c\n)", 0),
            "(?x:a (?-x: #lit )b \n)"
        );
        // Verbose does NOT leak out of a scoped group: after `(?x:…)` closes, a later `#` is
        // literal again (the bitset stays 0).
        assert_eq!(
            strip_screening_comments("(?x:a #c\n)b # not stripped", 0),
            "(?x:a \n)b # not stripped"
        );
        // A bare `(?:…)` / lookbehind `(?<=…)` are ordinary groups, not flag groups — copied
        // verbatim, and they do not enable verbose (the `#` after stays literal at bitset 0).
        assert_eq!(strip_screening_comments("(?:a)#c\n", 0), "(?:a)#c\n");
    }

    /// #364 corrective: the two semantic screens, run on the
    /// [`strip_screening_comments`] view, no longer fire on a `(?<x>` / `\N{…}` that lives
    /// *inside* a comment, while a real one in regex position still trips them. This pins
    /// the helper composition at the unit level (the end-to-end pins live in
    /// `tests/test_bounty_findings_h5.rs`).
    #[test]
    fn screens_skip_comment_text_on_stripped_view() {
        use flags::VERBOSE;
        let angle_ok = |src: &str, flags: u32| {
            reject_regex_crate_angle_named_group(&strip_screening_comments(src, flags)).is_ok()
        };
        let nuni_ok = |src: &str, flags: u32| {
            reject_named_unicode_escape(&strip_screening_comments(src, flags)).is_ok()
        };
        // Comment text — must pass (not screened).
        assert!(
            angle_ok(r"a(?#(?<x>)b", 0),
            "(?<x> inside (?#…) is comment text"
        );
        assert!(
            nuni_ok(r"a(?#\N{BULLET})b", 0),
            "named-unicode escape inside (?#…) is comment text"
        );
        assert!(
            angle_ok("a # (?<x>\nb", VERBOSE),
            "(?<x> inside verbose # … is comment text"
        );
        assert!(
            nuni_ok("a # \\N{BULLET}\nb", VERBOSE),
            "named-unicode escape inside verbose # … is comment text"
        );
        // Real constructs in regex position — must still be caught.
        assert!(
            !angle_ok(r"a(?<x>)b", 0),
            "a real angle group must still reject"
        );
        assert!(
            !nuni_ok(r"a\N{BULLET}b", 0),
            "a real named-unicode escape must still re-bucket"
        );
        // A verbose `#` is literal when VERBOSE is off, so a real `(?<x>` after it is still
        // a real group (the `#` does not hide it).
        assert!(
            !angle_ok("a#(?<x>)b", 0),
            "no VERBOSE: # is literal, the (?<x> is real"
        );
    }

    /// #481 differential audit: the five dialect screens now all drive ONE shared
    /// class-aware cursor ([`RegexCursor`]), so they must agree on the class-boundary
    /// semantics exactly as the five hand-rolled copies did — no accept/reject decision
    /// may widen. This pins the adversarial class/escape edges the standing scanner
    /// banks under-sample (leading `]`/`^` class members, escapes in and out of a class,
    /// octal runs straddling a class boundary, a flag-group / quantifier-looking
    /// construct *inside* `[…]` where it is a literal). Each expectation matches the
    /// pre-unification behavior verbatim; `test_scanner_differential` is the 3-way oracle
    /// that the *match outcome* is unchanged.
    #[test]
    fn unified_class_cursor_preserves_screen_decisions() {
        // The cursor itself: a `[`, optional `^`, optional leading literal `]`, then
        // close — and an escape pair is never a class boundary.
        let steps = |src: &str| -> Vec<(bool, Step)> {
            let chars: Vec<char> = src.chars().collect();
            let mut cur = RegexCursor::new(&chars);
            let mut out = Vec::new();
            while !cur.at_end() {
                let before = cur.in_class();
                out.push((before, cur.step()));
            }
            out
        };
        // `[]a]` — the leading `]` is a literal member, so the class spans `[]a]` and the
        // FIRST `]` does NOT close it; the SECOND `]` does.
        let s = steps("[]a]");
        assert_eq!(
            s[0].1,
            Step::ClassOpen { span: 2 },
            "`[]` opens, `]` literal"
        );
        assert!(s[1].0, "`a` is in-class");
        assert_eq!(s.last().unwrap().1, Step::ClassClose, "second `]` closes");
        // `[^]b]` — `^` then leading literal `]`, class spans the whole thing.
        assert_eq!(
            steps("[^]b]")[0].1,
            Step::ClassOpen { span: 3 },
            "`[^]` opens with negation + literal `]`"
        );
        // An escaped `\]` inside a class is a literal, not the close (escape step).
        let s = steps(r"[\]]");
        assert!(
            matches!(s[1].1, Step::Escape { esc: Some(']') }),
            "`\\]` is an escape pair, not the class close"
        );
        assert_eq!(s[2].1, Step::ClassClose, "the real `]` closes");

        // ── Cross-screen accept/reject parity on class-context edges. Inside `[…]` a
        // `+`/`{2}`/`(?i)` is a literal member, never a quantifier/flag group; an octal
        // run is octal in BOTH contexts; `\<`/`\>` and in-class `\b` normalize the same.
        // (rejected, accepted) per screen, grounded to the pre-#481 behavior.

        // Quantifier screen: literal `+`/`{2}` in a class accepted; real stacked/possessive
        // out-of-class rejected; comment transparency unchanged.
        for ok in ["[a+]", "[a{2}]", "[+*?]", "a[+]+", "[]+]"] {
            assert!(
                reject_quantifier_dialect_divergence(ok).is_ok(),
                "{ok:?}: a `+`/`{{}}` inside a class is a literal — must not be a multiple repeat"
            );
        }
        for bad in ["a++", "a{2}{3}", "[a]++", "[a]{2}{3}", "a+(?#c)?"] {
            assert!(
                reject_quantifier_dialect_divergence(bad).is_err(),
                "{bad:?}: a real possessive/stacked quantifier (out of class) must be refused"
            );
        }

        // Global-flag-group screen: `(?i)` inside a class is a literal; a real one (even
        // right after a class) is detected.
        assert!(
            find_global_inline_flag_group("[(?i)]").is_none(),
            "`(?i)` inside `[…]` is a literal class, not a flag group"
        );
        assert!(
            find_global_inline_flag_group("[abc](?i)x").is_some(),
            "a real `(?i)` after a class must still be detected"
        );

        // Out-of-range octal: rejected identically in and out of a class; a class
        // boundary does not hide a following out-of-range run.
        for bad in [r"\401", r"[\401]", r"[a]\500", r"[\477x]"] {
            assert!(
                reject_out_of_range_octal(bad).is_err(),
                "{bad:?}: an out-of-range octal must be refused in or out of a class"
            );
        }
        for ok in [r"\101", r"[\377]", r"[a\1b]"] {
            assert!(
                reject_out_of_range_octal(ok).is_ok(),
                "{ok:?}: an in-range octal / in-class backref must pass"
            );
        }

        // regex-crate-only escapes: class context is irrelevant (rejected both ways) — the
        // shared escape-pair walk must catch `\p`/`\z`/`\x{` inside `[…]` too.
        for bad in [r"[\p{L}]", r"[\za-z]", r"[\x{41}]", r"a\pLb"] {
            assert!(
                reject_regex_crate_only_dialect(bad).is_err(),
                "{bad:?}: a regex-crate-only escape is rejected in and out of a class"
            );
        }

        // normalize: in-class octal/`\b`, the `\<`/`\>` rewrite (ADR-0004), and a leading
        // literal `]` are all byte-identical to the pre-#481 output.
        assert_eq!(normalize_python_escapes(r"[\b\101]"), r"[\x08\x41]");
        assert_eq!(normalize_python_escapes(r"\<\>"), "<>");
        assert_eq!(
            normalize_python_escapes(r"[]\1<]"),
            r"[]\x01<]",
            "leading `]` literal, in-class octal, and a bare `<` preserved"
        );
        // `\<` INSIDE a class also normalizes to the bare char (ADR-0004 dotmotif case).
        assert_eq!(normalize_python_escapes(r"[\<\>]"), "[<>]");
    }

    /// #501 differential audit: the two *sibling* walkers left out of #481's scope —
    /// [`strip_screening_comments`] and [`reject_regex_crate_angle_named_group`] — now also
    /// drive the shared class-/escape-aware [`RegexCursor`]. This pins the class/escape
    /// edges the standing scanner banks under-sample (ADR-0021), each grounded **verbatim**
    /// to the pre-#501 behaviour: no strip-output or accept/reject decision may change.
    /// (`test_scanner_differential` is the 3-way oracle that the end-to-end *match outcome*
    /// is unchanged; this pins the unit-level class-boundary semantics.)
    #[test]
    fn unified_cursor_preserves_sibling_walker_decisions() {
        use flags::VERBOSE;

        // ── strip_screening_comments: the class-/escape-aware edges now owned by the cursor.
        // A leading `]` (after an optional `^`) is a literal class member — a `#`/`(?#`
        // inside that class stays literal (the class did not close early).
        assert_eq!(
            strip_screening_comments("[]#(?#]a", VERBOSE),
            "[]#(?#]a",
            "leading `]` keeps the class open; in-class `#`/`(?#` are literal members"
        );
        assert_eq!(
            strip_screening_comments("[^]#x]y # c\n", VERBOSE),
            "[^]#x]y \n",
            "`[^]…]` literal leading `]`; only the OUT-of-class verbose `#` strips"
        );
        // An escaped `\]` inside a class is a literal, not the close — the class stays open,
        // so a following `#` is still an in-class literal under VERBOSE.
        assert_eq!(
            strip_screening_comments(r"[\]#](?#c)z", VERBOSE),
            r"[\]#]z",
            r"`\]` is not the class close; the in-class `#` is literal, the out `(?#c)` strips"
        );
        // An escaped `\(` / `\#` is never a comment open / verbose comment — copied verbatim.
        assert_eq!(strip_screening_comments(r"a\(?#c)b", 0), r"a\(?#c)b");
        assert_eq!(strip_screening_comments(r"a\#b", VERBOSE), r"a\#b");
        // Out-of-class `(?#…)` strips in any scope; a real verbose `#` strips to EOL.
        assert_eq!(
            strip_screening_comments("a(?#c)b # d\ne", VERBOSE),
            "ab \ne"
        );

        // ── reject_regex_crate_angle_named_group: class-/escape-aware `(?<` detection.
        // A real angle named group out-of-class is rejected; the lookbehind forms pass.
        assert!(reject_regex_crate_angle_named_group("a(?<x>)b").is_err());
        assert!(reject_regex_crate_angle_named_group("a(?<name>)b").is_err());
        assert!(reject_regex_crate_angle_named_group("a(?<=b)c").is_ok());
        assert!(reject_regex_crate_angle_named_group("a(?<!b)c").is_ok());
        // `(?P<name>…)` is the Python-accepted form — never a `(?<` opener.
        assert!(reject_regex_crate_angle_named_group("(?P<n>x)").is_ok());
        // Inside `[…]` a `(?<` is a literal class member (Python reads `[(?<x>]` as plain
        // chars) — must NOT reject. Leading `]`/`^` and an escaped `\(` keep the class /
        // escape tracking honest.
        assert!(
            reject_regex_crate_angle_named_group("[(?<x>]z").is_ok(),
            "`(?<` inside a class is literal — must not reject"
        );
        assert!(
            reject_regex_crate_angle_named_group("[](?<x>]z").is_ok(),
            "leading `]` keeps the class open over the `(?<`"
        );
        assert!(
            reject_regex_crate_angle_named_group(r"a\(?<x>b").is_ok(),
            r"an escaped `\(` is not a group open"
        );
        // A real angle group right AFTER a class still rejects (the class closed first).
        assert!(reject_regex_crate_angle_named_group("[abc](?<x>)").is_err());
    }
}
