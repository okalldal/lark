//! Test infrastructure for the L2 bounded-lookaround lowering harness
//! (`docs/LEXER_DFA_PLAN.md`, "Verification harness"). This is the *net* the lowering
//! is gated by — the lowering itself lives in `src/lookaround/`. All three shapes have
//! landed (M1 trailing, M2 leading, M3 bounded-lookbehind), so the per-shape gates that
//! consume this infra are active. It provides, all deterministic (seeded/exhaustive
//! enumeration, fixed order):
//!
//!   * **Generators** — per supported shape, hundreds of concrete terminals
//!     ([`supported_terminals`]); an out-of-shape adversarial corpus
//!     ([`reject_cases`]); and exhaustive small-alphabet input corpora
//!     ([`corpus`]).
//!   * **The `fancy-regex` oracle** — [`fancy_prefix`], the independent
//!     match-length reference the lowering is verified against (kept forever as a
//!     dev/test dependency, per the plan).
//!   * **The mutation framework** — deliberately-wrong [`Classifier`]s
//!     ([`reject_path_mutants`]) for the reject path, plus the boundary
//!     ([`boundary_mutations`]) and lookbehind ([`lookbehind_mutations`])
//!     equivalence-layer mutants the per-shape meta-tests drive.
//!   * **The lowered-matcher hook** — [`lowered_prefix`], which drives the real
//!     lowered `DfaScanner` (a one-terminal grammar under `LexerBackend::Dfa`, probed
//!     at offset 0). It returns `Err` only when the lowering *declines* a terminal (a
//!     non-greedy-monotone base, a variable-offset lookbehind), so a comparison run
//!     against a declined terminal fails loudly rather than passing for the wrong
//!     reason.
#![allow(dead_code)]

use lark_rs::{classify, Classification, Classifier, Rejection, ShapeClass, Verdict};

// ─── Generated-terminal model ──────────────────────────────────────────────────

/// A generated terminal: its pattern, the shape it is supposed to be, and the
/// quotient alphabet + bound for its exhaustive input corpus.
#[derive(Debug, Clone)]
pub struct GenTerminal {
    pub name: String,
    pub pattern: String,
    pub shape: ShapeClass,
    pub alphabet: Vec<char>,
    pub max_len: usize,
}

/// A generated out-of-shape terminal and the rejection reason the classifier must
/// give it. The dangerous direction (false-accept) is exactly what the reject
/// corpus and the mutation meta-test guard.
#[derive(Debug, Clone)]
pub struct RejectCase {
    pub name: String,
    pub pattern: String,
    pub expected: Rejection,
}

// ─── Input corpora ─────────────────────────────────────────────────────────────

/// Every string over `alphabet` of length `0..=max_len`, in a fixed deterministic
/// order (the same enumerator `tests/test_lookaround.rs::matchlen` uses).
pub fn corpus(alphabet: &[char], max_len: usize) -> Vec<String> {
    let mut all = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..max_len {
        let mut next = Vec::with_capacity(frontier.len() * alphabet.len());
        for s in &frontier {
            for &c in alphabet {
                let mut t = s.clone();
                t.push(c);
                next.push(t);
            }
        }
        all.extend_from_slice(&next);
        frontier = next;
    }
    all
}

// ─── The fancy-regex oracle (independent reference) ─────────────────────────────

/// Compile `pattern` into the anchored `fancy-regex` oracle matcher (anchored at the
/// start with `\A`), or `None` if it does not compile. Compile **once** per terminal
/// and reuse across the corpus — recompiling per input is what makes the generative
/// layer pathologically slow.
pub fn fancy_matcher(pattern: &str) -> Option<fancy_regex::Regex> {
    fancy_regex::Regex::new(&format!(r"\A(?:{pattern})")).ok()
}

/// Anchored matched-prefix length (in **characters**) of a precompiled oracle
/// matcher at the start of `input`, or `None` for no match at offset 0. This is the
/// independent `fancy-regex` reference the lowering is verified against.
///
/// A **zero-width** match (`m.end() == 0`) is reported as `None`: lark's scanner
/// requires `m.end() > pos` and Python Lark forbids zero-width terminals outright
/// (`min_width == 0`), so a length-0 match is never a token lark emits. Mapping it to
/// `None` makes this raw-`fancy-regex` reference model lark's scanner semantics — the
/// same kind of adaptation as the `m.start() == 0` anchoring above — so a nullable
/// base like `[0-9]*(?=S)` (which lark would reject at build) is compared faithfully.
pub fn fancy_prefix(re: &fancy_regex::Regex, input: &str) -> Option<usize> {
    match re.find(input) {
        Ok(Some(m)) if m.start() == 0 && m.end() > 0 => Some(input[..m.end()].chars().count()),
        _ => None,
    }
}

// ─── The lowered-matcher hook ──────────────────────────────────────────────────

/// Anchored matched-prefix length (in **characters**) of the *lowered* terminal at
/// the start of `input`, or `Ok(None)` for no match — the real lowered `DfaScanner`,
/// driven through the public lexer API so the harness stays at the `match_at`
/// boundary (engine-agnostic). The terminal is built into a **one-terminal** grammar
/// under [`LexerBackend::Dfa`] and probed with `BasicLexer::match_at(input, 0)`: the
/// scanner's anchored match at offset 0 *is* the lowered prefix (or `None`). Matching
/// only at offset 0 — rather than lexing the whole input — keeps the exhaustive
/// generative corpus tractable.
///
/// Returns `Err` only when the lowering *declines* the terminal (a non-greedy-monotone
/// base, a variable-offset lookbehind) or *rejects* it (out-of-shape), so a per-shape
/// equivalence layer run against a declined terminal fails loudly rather than passing
/// for the wrong reason.
pub fn lowered_prefix(_name: &str, pattern: &str, input: &str) -> Result<Option<usize>, String> {
    let lexer = lowered_lexer(pattern)?;
    if input.is_empty() {
        return Ok(None);
    }
    // The single-terminal scanner matches only `TOK`, so any anchored match at 0 is
    // the lowered terminal's; its char length is the lowered prefix.
    Ok(lexer.match_at(input, 0).map(|(_, v)| v.chars().count()))
}

thread_local! {
    /// Per-pattern cache of the built one-terminal Dfa lexer, so a terminal's whole
    /// corpus reuses one dense DFA. `Rc` because the cache outlives each borrow.
    static LOWERED_LEXERS: std::cell::RefCell<
        std::collections::HashMap<String, std::rc::Rc<lark_rs::BasicLexer>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Build (or fetch from the thread-local cache) the one-terminal `LexerBackend::Dfa`
/// lexer for `pattern` — a single terminal `TOK` over the lowered pattern, probed at
/// offset 0.
fn lowered_lexer(pattern: &str) -> Result<std::rc::Rc<lark_rs::BasicLexer>, String> {
    use lark_rs::{
        basic_lexer_conf, load_grammar, lower, lower_terminal, BasicLexer, LexerBackend,
    };

    if let Some(l) = LOWERED_LEXERS.with(|c| c.borrow().get(pattern).cloned()) {
        return Ok(l);
    }

    // A terminal the lowering declines/rejects is surfaced as an error, not a silent
    // mismatch.
    lower_terminal("TOK", pattern).map_err(|e| format!("lowering rejected `{pattern}`: {e}"))?;

    // Inside `/…/`, an unescaped `/` must be escaped as `\/` (mirrors the differential
    // grammar builder); an already-`\/` is left alone.
    let mut escaped = String::with_capacity(pattern.len());
    let mut prev_backslash = false;
    for ch in pattern.chars() {
        if ch == '/' && !prev_backslash {
            escaped.push('\\');
        }
        escaped.push(ch);
        prev_backslash = ch == '\\' && !prev_backslash;
    }
    let grammar = format!("start: TOK\nTOK: /{escaped}/\n");

    let g = load_grammar(&grammar, &["start".to_string()], false, false)
        .map_err(|e| format!("load_grammar failed: {e:?}"))?;
    let cg = lower(&g);
    let conf = basic_lexer_conf(&cg, 0).with_backend(LexerBackend::Dfa);
    let lexer = std::rc::Rc::new(
        BasicLexer::new(&conf).map_err(|e| format!("BasicLexer build failed: {e:?}"))?,
    );
    LOWERED_LEXERS.with(|c| c.borrow_mut().insert(pattern.to_string(), lexer.clone()));
    Ok(lexer)
}

// ─── Supported-shape generators ────────────────────────────────────────────────

/// Decode an escaped *literal* trigger char `\X` into the char it stands for, or
/// `None` if `X` is a character *class* (`\d`, `\w`, `\b`, …) rather than a literal.
/// This is what lets an escaped guard char (`\.`, `\\`, `\/`) enter the corpus —
/// without it, a guard whose trigger is escaped is never exercised and a lowering
/// that ignores the guard would pass vacuously (the `DEC_NUMBER`-length-change lesson
/// the plan invokes, reappearing in the generator).
fn decode_escape_literal(x: char) -> Option<char> {
    match x {
        'n' => Some('\n'),
        't' => Some('\t'),
        'r' => Some('\r'),
        // Class / assertion escapes — *not* literals, no single trigger char.
        c if c.is_ascii_alphanumeric() => None,
        // Everything else (`.`, `\`, `/`, `"`, `'`, `(`, `-`, …) is a literal.
        c => Some(c),
    }
}

/// A deterministic alphabet for a generated terminal. **Trigger literals first**:
/// the literal chars the pattern names — *including escaped ones* (`\.` → `.`,
/// `\\` → `\`) — so a guard's trigger always enters the exhaustive corpus and a
/// lowering that ignores the guard cannot pass vacuously. Then the generic
/// representatives `['a','x','0']` (so a match and a miss are always reachable),
/// deduplicated and capped. The trigger-first order is what keeps the cap from
/// crowding out the load-bearing char.
fn quotient_alphabet(pattern: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let mut push = |c: char, out: &mut Vec<char>| {
        if !out.contains(&c) {
            out.push(c);
        }
    };

    // Pass 1 — literal characters named in the pattern, decoding escapes, FIRST.
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                if let Some(x) = chars.get(i + 1).copied().and_then(decode_escape_literal) {
                    push(x, &mut out);
                }
                i += 2; // skip the escape pair either way
            }
            // Regex metacharacters carry no literal trigger of their own.
            '(' | ')' | '[' | ']' | '{' | '}' | '?' | '!' | '=' | '<' | '>' | '|' | '*' | '+'
            | '.' | '^' | '$' | '-' | ':' => i += 1,
            _ => {
                push(c, &mut out);
                i += 1;
            }
        }
    }
    // Pass 2 — generic representatives so every corpus exercises a "match" and a
    // "miss" even when the pattern names no plain literal.
    for c in ['a', 'x', '0'] {
        push(c, &mut out);
    }

    out.truncate(5); // small enough that the exhaustive corpus stays tractable
    out
}

fn gen(name: String, pattern: String, shape: ShapeClass) -> GenTerminal {
    let alphabet = quotient_alphabet(&pattern);
    GenTerminal {
        name,
        pattern,
        shape,
        alphabet,
        max_len: 5,
    }
}

/// The supported-terminal population: hundreds of concrete terminals across the
/// three shapes, varying base pattern, char-set, width, and quote/escape/delimiter
/// content. Deterministic and stably ordered (the names are sequential).
pub fn supported_terminals() -> Vec<GenTerminal> {
    let mut out = Vec::new();
    let mut n = 0usize;
    let mut next_name = move || {
        let s = format!("GEN{n}");
        n += 1;
        s
    };

    // --- Leading-boundary lookahead: `(?=S)BODY` / `(?!S)BODY`. The assertion is the
    //     first element of the top-level concat. S is a bounded guard. ---
    let lead_guards = [
        "--",
        "if",
        "0x",
        "ab|cd",
        "[0-9]",
        "[A-Z]",
        r"x{2}",
        r"\.",
        "if|else|while",
    ];
    let lead_bodies = [
        r"[a-z]+",
        r"\w+",
        r"[A-Za-z_]+",
        r"[0-9]+",
        r"[a-z][a-z0-9]*",
    ];
    for g in lead_guards {
        for b in lead_bodies {
            for neg in ["=", "!"] {
                out.push(gen(
                    next_name(),
                    format!("(?{neg}{g}){b}"),
                    ShapeClass::LeadingBoundary,
                ));
            }
        }
    }

    // --- Trailing-boundary lookahead: `BODY(?=S)` / `BODY(?!S)`. The assertion is the
    //     last element. Includes the bundled OP/DEC_NUMBER shapes + operator edges. ---
    let trail_bodies = [
        r"[0-9]+", r"[a-z]+", r"=", r":", r"\w+", r"[0-9]*", r"0", r"[?]",
    ];
    let trail_guards = ["[0-9]", "[a-z]", "=|>", ":", "[A-Za-z]", "[1-9]", r"\d"];
    for b in trail_bodies {
        for g in trail_guards {
            for neg in ["=", "!"] {
                out.push(gen(
                    next_name(),
                    format!("{b}(?{neg}{g})"),
                    ShapeClass::TrailingBoundary,
                ));
            }
        }
    }

    // --- Bounded lookbehind: leading `(?<!S)BODY` and internal `A(?<!S)B`, with a
    //     fixed-width body. Includes the escape/delimiter idioms. ---
    let behind_guards = [r"\\", "_", "==", "abc", "[a-z]", r"x{2}", "ab", r"\."];
    let behind_pre = ["", "a", r"\w"];
    let behind_post = [r"/", r"[a-z]", r"\w+", "x"];
    for g in behind_guards {
        for pre in behind_pre {
            for post in behind_post {
                for neg in ["=", "!"] {
                    out.push(gen(
                        next_name(),
                        format!("{pre}(?<{neg}{g}){post}"),
                        ShapeClass::BoundedLookbehind,
                    ));
                }
            }
        }
    }

    // --- Quote / escape / delimiter content. String- and regex-literal idioms whose
    //     guard sits cleanly at a top-level boundary (the bundled STRING's *nested*
    //     guard is a first-shape refinement, so it is represented here by its
    //     boundary-form cousins). Exercises `"`, `'`, `/`, `\` in body + guard. ---
    let delim_trailing = [
        r#""[^"]*"(?!")"#,     // a closed string, not followed by another quote
        r#"'[^']*'(?!')"#,     // single-quote variant
        r#"/[^/]*/(?![a-z])"#, // a regex literal not followed by a flag letter
        r#"\d+(?!\.)"#,        // an integer not followed by a decimal point
    ];
    for p in delim_trailing {
        out.push(gen(
            next_name(),
            p.to_string(),
            ShapeClass::TrailingBoundary,
        ));
    }
    let delim_leading = [
        r#"(?!//)/[^/]+"#,  // a regex body that is not the empty `//`
        r#"(?!""")"[^"]*"#, // a short-string opener that is not the long `"""`
        r#"(?=")[^a-z]+"#,  // content that must start at a quote
    ];
    for p in delim_leading {
        out.push(gen(next_name(), p.to_string(), ShapeClass::LeadingBoundary));
    }
    let delim_behind = [
        r#"(?<!\\)""#,  // a quote not preceded by a backslash (string close)
        r#"(?<!\\)/"#,  // a slash not preceded by a backslash (regex delimiter)
        r#"a(?<=\\)'"#, // a quote that *is* escaped
    ];
    for p in delim_behind {
        out.push(gen(
            next_name(),
            p.to_string(),
            ShapeClass::BoundedLookbehind,
        ));
    }

    // --- Biting lookbehind cases (the generator-vacuity fix the plan calls for). A
    //     leading lookbehind is vacuous at offset 0 (nothing precedes pos), so the
    //     guard is placed where it actually *bites within an offset-0 match*: a
    //     **variable preceding class containing the trigger** (`\w(?<!_)x` rejects
    //     `_x`, accepts `ax`). The escaped-trigger `quotient_alphabet` decoding puts
    //     the trigger in the corpus, so a lowering that *ignored* the lookbehind would
    //     diverge here — it cannot pass vacuously. ---
    let behind_biting = [
        r"\w(?<!_)x",       // negative, trigger `_` ∈ the preceding `\w` class
        r"\w(?<=_)x",       // positive: the preceding char *must* be `_`
        r"[a\\](?<!\\)x",   // backslash parity — the canonical LONG_STRING flavor
        r"[ab](?<=a)x",     // positive, narrow preceding class
        r"[a-c](?<![ab])x", // multi-char negative trigger class
    ];
    for p in behind_biting {
        out.push(gen(
            next_name(),
            p.to_string(),
            ShapeClass::BoundedLookbehind,
        ));
    }

    out
}

/// The **string-literal opening-guard idiom** population (python.STRING's *actual*
/// nested/prefixed shape — `docs/LEXER_DFA_PLAN.md`, the marquee L2 splice). Unlike the
/// bare `(?!S)X` leading cousins in [`supported_terminals`], these carry the guard
/// **after a variable-width prefix + the opening quote**, with the lazy escaped body and
/// the `(?<!\\)` lookbehind — the genuinely-new shape. Each varies the prefix (none /
/// bounded / the bundled alternation), the quote kind, and the arm count, so the splice
/// is exercised across the composition surface (prefix × body × lookbehind). A bespoke
/// alphabet ensures the corpus exercises the `""""` boundary, escaped quotes, and escaped
/// backslashes — the cases a forgotten guard or a wrong body normalization gets wrong.
///
/// The shape carries *both* a leading-boundary guard (the `(?!"")`) and bounded
/// lookbehinds, so its `shape` field is the headline [`ShapeClass::LeadingBoundary`];
/// these are kept out of [`supported_terminals`] (whose per-shape gates assume a single
/// shape per terminal) and driven by their own dedicated equivalence + mutation gates.
pub fn string_idiom_terminals() -> Vec<GenTerminal> {
    let dq = r#""(?!"").*?(?<!\\)(\\\\)*?""#;
    let sq = r#"'(?!'').*?(?<!\\)(\\\\)*?'"#;
    // A non-quote literal delimiter (`/`) — the splice construction is delimiter-agnostic,
    // so exercising a `/`-delimited idiom guards that generality (not just `"`/`'`).
    let slash = r#"/(?!//).*?(?<!\\)(\\\\)*?/"#;
    let both = format!("{dq}|{sq}");
    let arms: [(&str, &str, &[char]); 4] = [
        ("dq", dq, &['"', '\\', 'a']),
        ("sq", sq, &['\'', '\\', 'a']),
        ("slash", slash, &['/', '\\', 'a']),
        ("both", &both, &['"', '\'', '\\', 'a']),
    ];
    let prefixes: [(&str, &[char]); 3] = [
        ("", &[]),
        ("(r?)", &['r']),
        ("([ubf]?r?|r[ubf])", &['r', 'b']),
    ];
    let mut out = Vec::new();
    let mut n = 0usize;
    for (plabel, pchars) in prefixes {
        for (alabel, arm_src, achars) in &arms {
            let pattern = format!("{plabel}({arm_src})");
            // Alphabet: the arm's distinguishing chars + the prefix letters, capped.
            let mut alphabet: Vec<char> = achars.to_vec();
            for &c in pchars {
                if !alphabet.contains(&c) {
                    alphabet.push(c);
                }
            }
            alphabet.truncate(5);
            out.push(GenTerminal {
                name: format!("STR_{plabel}_{alabel}_{n}"),
                pattern,
                shape: ShapeClass::LeadingBoundary,
                alphabet,
                max_len: 5,
            });
            n += 1;
        }
    }
    out
}

/// The bundled `lark.REGEXP` pattern, verbatim — the Stage-B **regex-literal idiom**
/// (`/ body / flags` with the internal `(?!\/)` empty-body guard). The recognizer
/// (`recognize_regexp_idiom`) is exact, so this is the *only* pattern in the idiom's
/// acceptance surface; the population below varies the corpus, not the pattern.
pub const REGEXP_RAW: &str = r#"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*"#;

/// The **regex-literal idiom** population (`lark.REGEXP`, Stage B). Unlike the other
/// generators, the acceptance surface is a single exact shape, so the population varies
/// the **exhaustive corpus** instead: slash/backslash-heavy alphabets (open/close + the
/// `//` boundary + escape pairing), with and without flag letters (`i`, `m` — including
/// the multi-flag suffix and the flag-vs-content ambiguity `x ∈ [imslux]` exercises),
/// and a content char. These corpora drive the lazy close, the dangling-escaped-slash
/// backtracking close (`/a\/b` → `/a\/`), the greedy flags suffix, and the `//` reject.
pub fn regexp_idiom_terminals() -> Vec<GenTerminal> {
    let alphabets: [(&str, &[char], usize); 3] = [
        // slash + backslash + content + one flag letter, the canonical mix
        ("core", &['/', '\\', 'a', 'i'], 6),
        // pure delimiter/escape stress (every string is slash/backslash noise)
        ("slashes", &['/', '\\', 'i'], 7),
        // two flag letters + content: multi-flag suffixes (`/a/im`) and flag runs
        ("flags", &['/', '\\', 'a', 'i', 'm'], 5),
    ];
    alphabets
        .into_iter()
        .map(|(label, alphabet, max_len)| GenTerminal {
            name: format!("REGEXP_{label}"),
            pattern: REGEXP_RAW.to_string(),
            shape: ShapeClass::LeadingBoundary,
            alphabet: alphabet.to_vec(),
            max_len,
        })
        .collect()
}

/// **Near-miss regexp-idiom shapes the recognizer must NOT accept** — its reject
/// surface. Each is the bundled idiom with exactly one pinned part changed: the
/// delimiter, the guard, a body alternative, the quantifier, the close, or the flags
/// suffix. None may lower (each would need its own proof; several are genuinely
/// out-of-shape). The recognizer-level assertion lives in
/// `src/lookaround/lower.rs::tests::regexp_idiom_recognizer_is_exact`; the route-level
/// one in `tests/test_regexp_splice.rs`.
pub fn regexp_idiom_reject_patterns() -> Vec<String> {
    [
        r"\#(?!\#)(\\\#|\\\\|[^\#])*?\#[imslux]*", // wrong delimiter
        r"\/(?!x)(\\\/|\\\\|[^\/])*?\/[imslux]*",  // guard body is not the close
        r"\/(?!\/)((?=a)\\\/|\\\\|[^\/])*?\/[imslux]*", // nested assertion in the body
        r"\/(?!\/)(.*?|\\\\|[^\/])*?\/[imslux]*",  // an unrelated lazy `.*?` body arm
        r"\/(?!\/)(\\\/|\\\\|[^\/])*\/[imslux]*",  // greedy body, not the lazy `*?`
        r"\/(?!\/)(\\\/|\\\\|[^\/])*?[imslux]*",   // missing the close slash
        r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[a-z]*",    // different flags suffix
        r"\/(?!\/)(\\\\|\\\/|[^\/])*?\/[imslux]*", // body alternatives reordered
        r"\/(?!\/)(\\\/|\\\\|\\n|[^\/])*?\/[imslux]*", // extra body alternative
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The bundled `python.LONG_STRING` pattern, verbatim (the `/is` flags live on the
/// terminal) — the Stage-B **long-string idiom** (`<prefix> <qqq> body <qqq>` with the
/// escape-parity `(?<!\\)(\\\\)*?` close, absorbed by the escape-pair body
/// normalization).
pub const LONG_STRING_RAW: &str =
    r#"([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')"#;

/// The **long-string idiom** population (`python.LONG_STRING`, Stage B). The acceptance
/// surface is narrow (triple-quote delimiters only), so the population varies the
/// structural forms the recognizer admits (prefix-less single arm, both quote kinds, the
/// full bundled two-arm + prefix shape) and the **exhaustive corpus**: quote/backslash
/// stress for the parity close and the overlapping-quote-run boundaries, a newline
/// alphabet for the **non-dotall** `\n`-exclusion (the one-terminal harness grammar is
/// unflagged, so these corpora run non-DOTALL — consistently on both the lowered and the
/// fancy-oracle side; the real `/is` path is covered by the splice canaries and the
/// backend differential in `test_long_string_splice.rs`), and an `r`-prefix alphabet for
/// the prefix interplay. Tokens need ≥ 6 chars (`""""""`), hence the longer max_len.
///
/// `shape` is [`ShapeClass::BoundedLookbehind`] — what the pattern's assertions actually
/// classify as (unlike STRING/REGEXP there is no lookahead to re-tag).
pub fn long_string_idiom_terminals() -> Vec<GenTerminal> {
    let dq_arm = r#"(""".*?(?<!\\)(\\\\)*?""")"#;
    let sq_arm = r#"('''.*?(?<!\\)(\\\\)*?''')"#;
    let cases: [(&str, &str, &[char], usize); 4] = [
        // prefix-less dq arm: quote/backslash/content stress to length 8
        ("dq", dq_arm, &['"', '\\', 'a'], 8),
        // sq twin
        ("sq", sq_arm, &['\'', '\\', 'a'], 8),
        // the non-dotall newline-exclusion corpus (a `\n` may appear nowhere in a body)
        ("nl", dq_arm, &['"', '\\', '\n'], 8),
        // the full bundled shape: prefix + both arms; `r""""""` is 7 chars
        ("full", LONG_STRING_RAW, &['"', '\'', '\\', 'r'], 7),
    ];
    cases
        .into_iter()
        .map(|(label, pattern, alphabet, max_len)| GenTerminal {
            name: format!("LONG_{label}"),
            pattern: pattern.to_string(),
            shape: ShapeClass::BoundedLookbehind,
            alphabet: alphabet.to_vec(),
            max_len,
        })
        .collect()
}

/// The wild-bank dotmotif `FLEXIBLE_KEY` pattern, verbatim — the **short-string
/// idiom** (idiom #4): a single-char-delimited token with a *non-empty* lazy escaped
/// body and the escape-parity `(?<!\\)(\\\\)*?` close, no opening guard.
pub const SHORT_STRING_RAW: &str = r#"(?:".+?(?<!\\)(\\\\)*?")|(?:'.+?(?<!\\)(\\\\)*?')"#;

/// The **short-string idiom** population (dotmotif `FLEXIBLE_KEY`, idiom #4). The
/// population varies the structural forms the recognizer admits (prefix-less single
/// arm, both quote kinds, the full wild two-arm shape) and the **exhaustive corpus**:
/// quote/backslash stress for the parity close, the guardless quote-leading-body
/// boundary (`"""` is one 3-char token, `""x"` one 4-char token, `""` no match —
/// where python.STRING's `(?!"")` would behave differently), and a newline alphabet
/// for the non-dotall `\n` exclusion. `shape` is [`ShapeClass::BoundedLookbehind`] —
/// what the pattern's assertions actually classify as (like LONG_STRING, there is no
/// lookahead to re-tag).
pub fn short_string_idiom_terminals() -> Vec<GenTerminal> {
    let dq_arm = r#"".+?(?<!\\)(\\\\)*?""#;
    let sq_arm = r#"'.+?(?<!\\)(\\\\)*?'"#;
    let cases: [(&str, &str, &[char], usize); 4] = [
        // prefix-less dq arm: quote/backslash/content stress
        ("dq", dq_arm, &['"', '\\', 'a'], 7),
        // sq twin
        ("sq", sq_arm, &['\'', '\\', 'a'], 7),
        // the non-dotall newline-exclusion corpus
        ("nl", dq_arm, &['"', '\\', '\n'], 7),
        // the full wild two-arm shape
        ("full", SHORT_STRING_RAW, &['"', '\'', '\\', 'a'], 6),
    ];
    cases
        .into_iter()
        .map(|(label, pattern, alphabet, max_len)| GenTerminal {
            name: format!("SHORT_{label}"),
            pattern: pattern.to_string(),
            shape: ShapeClass::BoundedLookbehind,
            alphabet: alphabet.to_vec(),
            max_len,
        })
        .collect()
}

/// **Near-miss short-string-idiom shapes the recognizer must NOT accept** — its
/// reject surface. Each is the wild shape with exactly one pinned part changed. The
/// headline near-miss is the **empty-capable `.*?` body without an opening guard**:
/// it closes at width 0 on `""` where the idiom's rewrite would consume a char, so it
/// must keep declining until someone proves its own rewrite (the section comment in
/// `src/lookaround/lower.rs` records this). The missing-lookbehind variant is
/// lookaround-*free* and classifies `Plain` ("not Branches" is the assertion, the
/// string-idiom reject convention). The recognizer-level assertion lives in
/// `src/lookaround/lower.rs::tests::short_string_idiom_recognizer_is_exact`.
pub fn short_string_idiom_reject_patterns() -> Vec<String> {
    [
        r#"".*?(?<!\\)(\\\\)*?""#, // empty-capable `.*?` body — the headline near-miss
        r#"".+?(\\\\)*?""#,        // missing the lookbehind (lookaround-free)
        r#"".+?(?<!x)(\\\\)*?""#,  // wrong lookbehind body
        r#"".+?(?<=\\)(\\\\)*?""#, // positive lookbehind
        r#"".+(?<!\\)(\\\\)*?""#,  // greedy `.+` body
        r#"".+?(?<!\\)(\\\\)*""#,  // greedy escape group
        r#"".+?(?<!\\)(\\)*?""#,   // wrong escape-group body
        r#"".+?(?<!\\)(\\\\)*?'"#, // mismatched open/close
        r#"ab.+?(?<!\\)(\\\\)*?b"#, // multi-char opener (not a single literal delimiter)
        r#".".+?(?<!\\)(\\\\)*?""#, // `.` before the opener (no longer the arm shape)
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// **Near-miss long-string-idiom shapes the recognizer must NOT accept** — its reject
/// surface. Each is the bundled idiom with exactly one pinned part changed: the
/// delimiter, the lookbehind, the escape group, or a quantifier's laziness. None may
/// lower (each would need its own proof; note the missing-lookbehind variant is
/// lookaround-*free* and so classifies `Plain` — "not Branches" is the assertion, same
/// as the string-idiom reject convention). The recognizer-level assertion lives in
/// `src/lookaround/lower.rs::tests::long_string_idiom_recognizer_is_exact`; the
/// route-level one in `tests/test_long_string_splice.rs`.
pub fn long_string_idiom_reject_patterns() -> Vec<String> {
    [
        r#"(r?)("".*?(?<!\\)(\\\\)*?"")"#, // two-quote delimiter
        r#"""".*?(?<!\\)(\\\\)*?'''"#,     // mismatched open/close
        r#"""".*?(\\\\)*?""""#,            // missing the lookbehind (lookaround-free)
        r#"""".*?(?<!x)(\\\\)*?""""#,      // wrong lookbehind body
        r#"""".*?(?<=\\)(\\\\)*?""""#,     // positive lookbehind
        r#"""".*(?<!\\)(\\\\)*?""""#,      // greedy `.*` body
        r#"""".*?(?<!\\)(\\\\)*""""#,      // greedy escape group
        r#"""".*?(?<!\\)(\\)*?""""#,       // wrong escape-group body
        r"\/\/\/.*?(?<!\\)(\\\\)*?\/\/\/", // tripled non-quote delimiter
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// **Adversarial string-idiom shapes with a *non-literal* delimiter** — the recognizer's
/// own acceptance surface (not just the classifier's). Each is structurally the string
/// idiom `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` but with `<q>` a regex construct that is
/// **not** a fixed single literal: `.` (any char), the anchors `\b` / `$`, and the class
/// escape `\d`. A delimiter like these cannot be faithfully emitted both bare (open/close)
/// and inside the negated body class, so the recognizer MUST decline them to `fancy-regex`
/// (reject-when-unsure). Lowering one would be a false-accept (and `\b` also breaks
/// build-parity). These are the witnesses for `recognizer_declines_non_literal_delimiters`.
pub fn string_idiom_reject_patterns() -> Vec<String> {
    // (delim-open, guard-body, delim-close) per arm, where the delimiter is non-literal.
    // The guard body must be `<delim><delim>` in source for the arm to be *shaped* like
    // the idiom (so the test exercises the delimiter gate, not some other mismatch).
    let arms: [(&str, &str); 4] = [
        (".", ".."),      // `.` — any char, not a fixed literal
        (r"\b", r"\b\b"), // `\b` — a zero-width word-boundary anchor
        (r"$", r"$$"),    // `$` — an end anchor
        (r"\d", r"\d\d"), // `\d` — a digit class, not a single char
    ];
    arms.into_iter()
        .map(|(open, guard)| {
            // The bundled wrapping: a bounded prefix + a single grouped arm.
            format!(r"(r?)({open}(?!{guard}).*?(?<!\\)(\\\\)*?{open})")
        })
        .collect()
}

// ─── Lowering mutants (the equivalence-layer mutation meta-test) ────────────────

/// A deliberately-wrong way to lower a **boundary** guard (leading or trailing). The
/// mutation meta-test asserts each one is *caught* — i.e. it produces a match-length
/// that diverges from `fancy-regex` somewhere on the boundary population, so a real
/// lowering that made the same mistake would turn the generative-equivalence layer
/// red. Each mirrors a concrete coding error the plan calls out
/// (`docs/LEXER_DFA_PLAN.md`, "Validate the harness itself — mutation meta-test").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryMutation {
    /// Forget the guard entirely — accept the base wherever it matches.
    ForgetGuard,
    /// Invert the guard's polarity — treat `(?!S)` as `(?=S)` and vice-versa.
    FlipPolarity,
    /// Drop the end-of-input case — require a following char on a *trailing* guard, so
    /// a token that runs to EOF is wrongly rejected (a negative trailing guard must
    /// hold at EOF). A no-op on a leading guard.
    DropTrailingEof,
}

/// Every boundary-lowering mutant the meta-test must catch.
pub fn boundary_mutations() -> [BoundaryMutation; 3] {
    [
        BoundaryMutation::ForgetGuard,
        BoundaryMutation::FlipPolarity,
        BoundaryMutation::DropTrailingEof,
    ]
}

fn render_guard(neg: bool, set: &str) -> String {
    if neg {
        format!("(?!{set})")
    } else {
        format!("(?={set})")
    }
}

/// Rebuild a (lookaround-bearing) reference pattern from the lowered branches with
/// `mutation` applied to each guard — the *wrong* lowering, expressed so the
/// independent `fancy-regex` engine can run it. The leading guard is re-emitted at the
/// branch front, the trailing guard at the branch end.
fn mutant_pattern(pattern: &str, mutation: BoundaryMutation) -> Option<String> {
    let branches = lark_rs::lookaround::lower::lower_boundary(pattern).ok()?;
    let arms: Vec<String> = branches
        .iter()
        .map(|b| {
            let lead = match &b.leading {
                None => String::new(),
                Some(g) => match mutation {
                    BoundaryMutation::ForgetGuard => String::new(),
                    BoundaryMutation::FlipPolarity => render_guard(!g.neg, &g.set),
                    // EOF-at-start is meaningless; leave the leading guard intact.
                    BoundaryMutation::DropTrailingEof => render_guard(g.neg, &g.set),
                },
            };
            let trail = match &b.trailing {
                None => String::new(),
                Some(g) => match mutation {
                    BoundaryMutation::ForgetGuard => String::new(),
                    BoundaryMutation::FlipPolarity => render_guard(!g.neg, &g.set),
                    BoundaryMutation::DropTrailingEof => {
                        format!("{}(?=[\\s\\S])", render_guard(g.neg, &g.set))
                    }
                },
            };
            format!("(?:{}{}{})", lead, b.regex, trail)
        })
        .collect();
    Some(arms.join("|"))
}

/// The compiled `fancy-regex` matcher for the **mutant** (wrongly-lowered) terminal,
/// or `None` if it does not compile. Built **once** per `(pattern, mutation)` and
/// reused across the corpus via [`fancy_prefix`] — compiling it per input is what
/// makes the mutation meta-test crawl.
pub fn mutant_matcher(pattern: &str, mutation: BoundaryMutation) -> Option<fancy_regex::Regex> {
    let mutated = mutant_pattern(pattern, mutation)?;
    fancy_matcher(&mutated)
}

/// Whether `pattern` carries at least one boundary guard (so a mutation is
/// observable).
pub fn has_guard(pattern: &str) -> bool {
    lark_rs::lookaround::lower::lower_boundary(pattern)
        .map(|bs| {
            bs.iter()
                .any(|b| b.leading.is_some() || b.trailing.is_some())
        })
        .unwrap_or(false)
}

// ─── Lookbehind lowering mutants (the M3 equivalence-layer mutation meta-test) ──

/// A deliberately-wrong way to lower a **bounded lookbehind** (`docs/LEXER_DFA_PLAN.md`,
/// "Validate the harness itself"). Each must be *caught* — diverge from the
/// `fancy-regex` oracle somewhere on the lookbehind population — so a real lowering
/// that made the same mistake would turn the bounded-lookbehind equivalence layer red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookbehindMutation {
    /// Ignore the lookbehind entirely — accept the base wherever it matches (the
    /// "ignore-the-lookbehind" mutant the plan names; vacuity is what would let it
    /// survive, which the biting generator cases defeat).
    IgnoreLookbehind,
    /// Forget the parity flip — treat `(?<!S)` as `(?<=S)` and vice-versa.
    FlipPolarity,
    /// Off-by-one window width — inspect one extra preceding char (`(?<!S)` →
    /// `(?<!.S)`), so the history window is the wrong size.
    OffByOneWidth,
}

/// Every lookbehind-lowering mutant the meta-test must catch.
pub fn lookbehind_mutations() -> [LookbehindMutation; 3] {
    [
        LookbehindMutation::IgnoreLookbehind,
        LookbehindMutation::FlipPolarity,
        LookbehindMutation::OffByOneWidth,
    ]
}

/// Whether `pattern` carries at least one bounded-lookbehind assertion.
pub fn has_lookbehind(pattern: &str) -> bool {
    use lark_rs::lookaround::{parse, Look, Node};
    fn walk(n: &Node) -> bool {
        match n {
            Node::Assertion {
                look: Look::Behind, ..
            } => true,
            Node::Assertion { body, .. } => walk(body),
            Node::Atom(_) => false,
            Node::Concat(parts) | Node::Alt(parts) => parts.iter().any(walk),
            Node::Group { body, .. } => walk(body),
        }
    }
    parse(pattern).map(|n| walk(&n)).unwrap_or(false)
}

/// Rebuild `pattern` with `mutation` applied to every bounded-lookbehind assertion —
/// the *wrong* lowering, re-expressed so the independent `fancy-regex` engine can run
/// it. Walks the parsed [`Node`] tree (so it is exact and structure-aware) and mutates
/// each `(?<…)` in place.
fn mutant_lookbehind_pattern(pattern: &str, mutation: LookbehindMutation) -> Option<String> {
    use lark_rs::lookaround::{parse, Look, Node};

    fn xform(node: &Node, m: LookbehindMutation) -> Option<Node> {
        match node {
            Node::Assertion {
                neg,
                look: Look::Behind,
                body,
                quant,
            } => match m {
                LookbehindMutation::IgnoreLookbehind => None, // drop it
                LookbehindMutation::FlipPolarity => Some(Node::Assertion {
                    neg: !neg,
                    look: Look::Behind,
                    body: body.clone(),
                    quant: quant.clone(),
                }),
                LookbehindMutation::OffByOneWidth => {
                    let wider = Node::Concat(vec![Node::Atom(".".to_string()), (**body).clone()]);
                    Some(Node::Assertion {
                        neg: *neg,
                        look: Look::Behind,
                        body: Box::new(wider),
                        quant: quant.clone(),
                    })
                }
            },
            // A forward assertion is left untouched.
            Node::Assertion { .. } => Some(node.clone()),
            Node::Atom(_) => Some(node.clone()),
            Node::Concat(parts) => Some(Node::Concat(
                parts.iter().filter_map(|p| xform(p, m)).collect(),
            )),
            Node::Alt(branches) => Some(Node::Alt(
                branches.iter().filter_map(|b| xform(b, m)).collect(),
            )),
            Node::Group { open, body, quant } => Some(Node::Group {
                open: open.clone(),
                body: Box::new(xform(body, m).unwrap_or(Node::Atom(String::new()))),
                quant: quant.clone(),
            }),
        }
    }

    let node = parse(pattern).ok()?;
    Some(xform(&node, mutation)?.to_source())
}

/// The compiled `fancy-regex` matcher for the **lookbehind-mutant** terminal, or
/// `None` if it does not compile. Built once per `(pattern, mutation)`.
pub fn mutant_lookbehind_matcher(
    pattern: &str,
    mutation: LookbehindMutation,
) -> Option<fancy_regex::Regex> {
    let mutated = mutant_lookbehind_pattern(pattern, mutation)?;
    fancy_matcher(&mutated)
}

// ─── Out-of-shape adversarial corpus ───────────────────────────────────────────

/// The adversarial reject corpus: out-of-shape assertions the classifier MUST
/// reject — unbounded lookahead, internal/priority-entangled lookahead, backref,
/// nested, and variable-width lookbehind. Each pairs the pattern with the exact
/// rejection reason expected.
pub fn reject_cases() -> Vec<RejectCase> {
    let mut out = Vec::new();
    let mut n = 0usize;
    let mut mk = |pattern: String, expected: Rejection, out: &mut Vec<RejectCase>| {
        out.push(RejectCase {
            name: format!("REJ{n}"),
            pattern,
            expected,
        });
        n += 1;
    };

    // Unbounded-width lookahead — the `*`/`+`/`{m,}` body, both polarities.
    // Leading unbounded lookaheads are now SUPPORTED (LeadingBoundary) — the guard
    // runs anchored at match-start so the assertion width does not affect the
    // accept position. Only TRAILING unbounded lookaheads remain in the reject
    // corpus.
    let unbounded_bodies = [
        "[ ]*X", "a*b", "ab+", r"\d{2,}", ".*", "(ab)+", "[0-9]+c", r"x*",
    ];
    for body in unbounded_bodies {
        for neg in ["=", "!"] {
            mk(format!("Y(?{neg}{body})"), Rejection::Unbounded, &mut out);
        }
    }

    // Internal / priority-entangled lookahead — mid-concat, or inside a repetition.
    let internal = [
        r"a(?=b)c",
        r"a(?!b)c",
        r"[0-9](?=x)[0-9]",
        r"\*(\*(?!/)|[^*])*\*/", // verilog MULTILINE_COMMENT
        r"(a(?!b))+c",
        r"foo(?=bar)baz",
    ];
    for p in internal {
        mk(p.to_string(), Rejection::Internal, &mut out);
    }

    // Backreference inside the assertion body — numeric and named/indexed escapes.
    let backref = [
        r"(a)(?=\1)b",
        r"(ab)(?!\1)c",
        r"(.)x(?=\1)",
        r"(?<n>a)(?=\k<n>)b",
        r"(a)(?!\g{1})b",
    ];
    for p in backref {
        mk(p.to_string(), Rejection::Backref, &mut out);
    }

    // Nested assertion.
    let nested = [
        r"(?=(?!a)b)c",
        r"(?!(?=x)y)z",
        r"a(?<!(?=q)w)b",
        r"(?=a(?!b))c",
    ];
    for p in nested {
        mk(p.to_string(), Rejection::Nested, &mut out);
    }

    // Variable-width lookbehind — unbounded history window.
    let var_behind = [r"(?<!a*)b", r"(?<!ab+)c", r"(?<=[0-9]+)x", r"a(?<!x*)b"];
    for p in var_behind {
        mk(p.to_string(), Rejection::VariableWidthBehind, &mut out);
    }

    // Quantifier on the assertion itself — degenerate, reject-when-unsure.
    let quantified = [r"(?=a)?[a-z]+", r"[0-9]+(?![0-9]){2}", r"(?!b)*x"];
    for p in quantified {
        mk(p.to_string(), Rejection::QuantifiedAssertion, &mut out);
    }

    out
}

// ─── Mutation framework ────────────────────────────────────────────────────────

/// Force a single assertion to a *supported*-looking verdict, by overwriting the
/// fields the real classifier keys on. The basis of the "wrongly accepts" mutants.
fn force_supported(
    mut info: lark_rs::lookaround::classify::AssertionInfo,
) -> lark_rs::lookaround::classify::AssertionInfo {
    use lark_rs::lookaround::classify::Position;
    use lark_rs::lookaround::Look;
    info.look = Look::Ahead;
    info.position = Position::Trailing;
    info.width = Some(1);
    info.has_backref = false;
    info.has_nested = false;
    info.has_quant = false;
    debug_assert!(matches!(info.verdict(), Verdict::Supported(_)));
    info
}

/// A named mutant classifier, for the mutation meta-test.
pub struct Mutant {
    pub name: &'static str,
    pub classifier: Box<dyn Classifier>,
}

/// A mutant that wrongly classifies *every* assertion as supported — the crudest
/// false-accept. The reject corpus must catch it.
struct AcceptEverything;
impl Classifier for AcceptEverything {
    fn classify(&self, pattern: &str) -> Result<Classification, lark_rs::GrammarError> {
        let mut c = classify(pattern)?;
        for a in &mut c.assertions {
            *a = force_supported(a.clone());
        }
        Ok(c)
    }
}

/// A mutant that keeps the real verdicts *except* it wrongly accepts one specific
/// rejection reason (turning it into a false boundary). One subtle-mistake mutant
/// per out-of-shape category, so the reject corpus must catch each category on its
/// own — not only via the crude `AcceptEverything`.
struct FlipReason(Rejection);
impl Classifier for FlipReason {
    fn classify(&self, pattern: &str) -> Result<Classification, lark_rs::GrammarError> {
        let mut c = classify(pattern)?;
        for a in &mut c.assertions {
            if a.verdict() == Verdict::Rejected(self.0) {
                *a = force_supported(a.clone());
            }
        }
        Ok(c)
    }
}

/// The mutants that should be caught **by the reject corpus** (each wrongly accepts
/// at least one out-of-shape assertion): the crude accept-everything plus one
/// per-reason "this category is fine" mistake, so every rejection reason is
/// independently defended. Equivalence-layer mutants (forget the parity flip,
/// off-by-one width, …) activate per shape in a later session.
pub fn reject_path_mutants() -> Vec<Mutant> {
    vec![
        Mutant {
            name: "accept-everything",
            classifier: Box::new(AcceptEverything),
        },
        Mutant {
            name: "internal-is-fine",
            classifier: Box::new(FlipReason(Rejection::Internal)),
        },
        Mutant {
            name: "unbounded-is-fine",
            classifier: Box::new(FlipReason(Rejection::Unbounded)),
        },
        Mutant {
            name: "var-behind-is-fine",
            classifier: Box::new(FlipReason(Rejection::VariableWidthBehind)),
        },
        Mutant {
            name: "backref-is-fine",
            classifier: Box::new(FlipReason(Rejection::Backref)),
        },
        Mutant {
            name: "nested-is-fine",
            classifier: Box::new(FlipReason(Rejection::Nested)),
        },
        Mutant {
            name: "quantified-is-fine",
            classifier: Box::new(FlipReason(Rejection::QuantifiedAssertion)),
        },
    ]
}

/// Run `classifier` over the reject corpus and return the names of any cases it
/// **fails to reject** (classifies as plain or fully-supported) — i.e. the
/// false-accepts. The real classifier returns an empty list; a mutant that
/// wrongly accepts returns a non-empty one. This is the single reusable check both
/// the active reject corpus and the mutation meta-test drive.
pub fn wrongly_accepted_rejects(classifier: &dyn Classifier, cases: &[RejectCase]) -> Vec<String> {
    let mut bad = Vec::new();
    for case in cases {
        match classifier.classify(&case.pattern) {
            // A reject case has at least one assertion; "accepted" = nothing rejected.
            Ok(c) if c.first_rejection().is_none() => bad.push(case.name.clone()),
            Ok(_) => {}  // rejected (good)
            Err(_) => {} // a parse error also counts as "not accepted"
        }
    }
    bad
}
