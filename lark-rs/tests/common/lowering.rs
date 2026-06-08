//! Test infrastructure for the L2 bounded-lookaround lowering harness
//! (`docs/LEXER_DFA_PLAN.md`, "Verification harness"). **No real lowering lives
//! here** — this is the *net* the lowering will be built against, one shape at a
//! time. It provides, all deterministic (seeded/exhaustive enumeration, fixed
//! order):
//!
//!   * **Generators** — per supported shape, hundreds of concrete terminals
//!     ([`supported_terminals`]); an out-of-shape adversarial corpus
//!     ([`reject_cases`]); and exhaustive small-alphabet input corpora
//!     ([`corpus`]).
//!   * **The `fancy-regex` oracle** — [`fancy_prefix`], the independent
//!     match-length reference the lowering is verified against (kept forever as a
//!     dev/test dependency, per the plan).
//!   * **The mutation framework** — deliberately-wrong [`Classifier`]s
//!     ([`reject_path_mutants`]) and the reusable reject-corpus check
//!     ([`wrongly_accepted_rejects`]) the mutation meta-test drives, validated now
//!     on the reject path.
//!   * **The lowered-matcher hook** — [`lowered_prefix`], stubbed to the *pending*
//!     state so the #[ignore]'d equivalence layers compile and fail loudly if
//!     un-ignored before a shape lands. The first-shape session points it at the
//!     real lowered `DfaScanner`.
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
pub fn fancy_prefix(re: &fancy_regex::Regex, input: &str) -> Option<usize> {
    match re.find(input) {
        Ok(Some(m)) if m.start() == 0 => Some(input[..m.end()].chars().count()),
        _ => None,
    }
}

// ─── The lowered-matcher hook (stubbed: pending) ───────────────────────────────

/// **Pending hook.** Anchored matched-prefix length of the *lowered* terminal at the
/// start of `input`, or `Ok(None)` for no match. Until a shape lands this returns
/// `Err` for every lookaround terminal, mirroring [`lark_rs::lower_terminal`]. The
/// generative-equivalence layer (`tests/test_lowering_equivalence.rs`) is
/// `#[ignore]`'d on this, and `.expect()`s it, so un-ignoring before the lowering
/// exists fails loudly rather than passing for the wrong reason.
///
/// The first-shape session replaces this body with the real lowered `DfaScanner`
/// (build a one-terminal grammar under `LexerBackend::Dfa`, match at offset 0) —
/// keeping the harness at the `match_at` boundary, engine-agnostic.
pub fn lowered_prefix(name: &str, _pattern: &str, _input: &str) -> Result<Option<usize>, String> {
    Err(format!(
        "lowered matcher for `{name}` is not implemented — pending first shape \
         (docs/LEXER_DFA_PLAN.md L2)"
    ))
}

// ─── Supported-shape generators ────────────────────────────────────────────────

/// A deterministic alphabet for a generated terminal: every literal-ish char that
/// appears in the pattern, plus a couple of generic representatives, deduplicated
/// and capped so the exhaustive corpus stays small. Order is fixed.
fn quotient_alphabet(pattern: &str) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let push = |c: char, out: &mut Vec<char>| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    // Generic representatives first so every corpus exercises a "match" and a "miss".
    for c in ['a', 'x', '0'] {
        push(c, &mut out);
    }
    // Literal characters from the pattern (skip regex metacharacters / escapes).
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => i += 2, // skip the escape pair
            '(' | ')' | '[' | ']' | '{' | '}' | '?' | '!' | '=' | '<' | '>' | '|' | '*' | '+'
            | '.' | '^' | '$' | '-' | ':' => i += 1,
            _ => {
                push(c, &mut out);
                i += 1;
            }
        }
    }
    out.truncate(4); // ≤4 distinct chars keeps the exhaustive corpus tractable
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

    out
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
    let unbounded_bodies = [
        "[ ]*X", "a*b", "ab+", r"\d{2,}", ".*", "(ab)+", "[0-9]+c", r"x*",
    ];
    for body in unbounded_bodies {
        for neg in ["=", "!"] {
            mk(format!("(?{neg}{body})Y"), Rejection::Unbounded, &mut out);
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

    // Backreference inside the assertion body.
    let backref = [r"(a)(?=\1)b", r"(ab)(?!\1)c", r"(.)x(?=\1)"];
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

/// A mutant that keeps the real verdicts *except* it turns an internal lookahead
/// into a (false) trailing boundary — the subtle "priority-entangled is fine"
/// mistake the plan warns about.
struct InternalIsTrailing;
impl Classifier for InternalIsTrailing {
    fn classify(&self, pattern: &str) -> Result<Classification, lark_rs::GrammarError> {
        let mut c = classify(pattern)?;
        for a in &mut c.assertions {
            if matches!(a.verdict(), Verdict::Rejected(Rejection::Internal)) {
                *a = force_supported(a.clone());
            }
        }
        Ok(c)
    }
}

/// A mutant that ignores unbounded width — treats an unbounded lookahead as a
/// bounded boundary assertion.
struct UnboundedIsFine;
impl Classifier for UnboundedIsFine {
    fn classify(&self, pattern: &str) -> Result<Classification, lark_rs::GrammarError> {
        let mut c = classify(pattern)?;
        for a in &mut c.assertions {
            if matches!(a.verdict(), Verdict::Rejected(Rejection::Unbounded)) {
                *a = force_supported(a.clone());
            }
        }
        Ok(c)
    }
}

/// A mutant that accepts variable-width lookbehind.
struct VarBehindIsFine;
impl Classifier for VarBehindIsFine {
    fn classify(&self, pattern: &str) -> Result<Classification, lark_rs::GrammarError> {
        let mut c = classify(pattern)?;
        for a in &mut c.assertions {
            if matches!(
                a.verdict(),
                Verdict::Rejected(Rejection::VariableWidthBehind)
            ) {
                *a = force_supported(a.clone());
            }
        }
        Ok(c)
    }
}

/// The mutants that should be caught **by the reject corpus** (each wrongly accepts
/// at least one out-of-shape assertion). Equivalence-layer mutants (forget the
/// parity flip, off-by-one width, …) activate per shape in a later session.
pub fn reject_path_mutants() -> Vec<Mutant> {
    vec![
        Mutant {
            name: "accept-everything",
            classifier: Box::new(AcceptEverything),
        },
        Mutant {
            name: "internal-is-trailing",
            classifier: Box::new(InternalIsTrailing),
        },
        Mutant {
            name: "unbounded-is-fine",
            classifier: Box::new(UnboundedIsFine),
        },
        Mutant {
            name: "var-behind-is-fine",
            classifier: Box::new(VarBehindIsFine),
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
