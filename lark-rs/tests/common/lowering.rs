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
//!   * **The lowered-matcher hook** — [`lowered_prefix`], which drives the real
//!     lowered `DfaScanner` (a one-terminal grammar under `LexerBackend::Dfa`, probed
//!     at offset 0) for the shapes that have landed, and returns `Err` for a shape
//!     still pending — so an equivalence layer un-ignored before its shape lands fails
//!     loudly rather than passing for the wrong reason.
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

// ─── The lowered-matcher hook (stubbed: pending) ───────────────────────────────

/// Anchored matched-prefix length (in **characters**) of the *lowered* terminal at
/// the start of `input`, or `Ok(None)` for no match — the real lowered `DfaScanner`,
/// driven through the public lexer API so the harness stays at the `match_at`
/// boundary (engine-agnostic). The terminal is built into a **one-terminal** grammar
/// under [`LexerBackend::Dfa`] and probed with `BasicLexer::match_at(input, 0)`: the
/// scanner's anchored match at offset 0 *is* the lowered prefix (or `None`). Matching
/// only at offset 0 — rather than lexing the whole input — keeps the exhaustive
/// generative corpus tractable.
///
/// Returns `Err` only when the lowering *rejects* the terminal (a shape not yet
/// lowered, or genuinely unsupported), so the per-shape equivalence layers fail
/// loudly if run against a pending shape rather than passing for the wrong reason.
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

    // A shape that does not lower yet is surfaced as an error, not a silent mismatch.
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

    out
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
