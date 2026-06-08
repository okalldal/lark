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
//!     ([`reject_path_mutants`]) for the reject path, plus the per-shape
//!     equivalence-layer mutants ([`trailing_mutants`] / [`trailing_mutant_survives`])
//!     that corrupt the *lowering itself* and must each be caught by the now-active
//!     terminal-level equivalence layer.
//!   * **The lowered-matcher hook** — [`lowered_prefix`], which delegates to the real
//!     `regex-automata` lowering ([`lark_rs::lowered_match_prefix`]) for landed shapes
//!     and returns `Err` for shapes whose lowering has not landed yet, so an
//!     un-ignored equivalence layer fails loudly rather than comparing the oracle
//!     against itself.
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

/// Anchored matched-prefix length (in **characters**) of the *lowered* terminal at
/// the start of `input`, or `Ok(None)` for no match — the lowered-path counterpart of
/// [`fancy_prefix`]. Delegates to [`lark_rs::lowered_match_prefix`], which builds the
/// terminal under the `regex-automata` lowering (no `fancy-regex`) and matches at
/// offset 0, returning the **raw** prefix length (zero-width included, exactly as
/// `fancy_prefix` does). It returns `Err` for any terminal whose shape has not landed,
/// so an un-ignored equivalence layer fails loudly rather than passing for the wrong
/// reason (it can never compare `fancy-regex` against itself).
pub fn lowered_prefix(name: &str, pattern: &str, input: &str) -> Result<Option<usize>, String> {
    lark_rs::lowered_match_prefix(name, pattern, input).map_err(|e| e.to_string())
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

// ─── Equivalence-layer mutants (per-shape: trailing boundary) ───────────────────
//
// The reject-path mutants above attack the *classifier* (false-accept). These attack
// the *lowering itself*: a deliberately-wrong lowered matcher must be caught by the
// now-active terminal-level equivalence layer (it diverges from `fancy-regex`). They
// are realised on the lowering's own `(body, guard)` decomposition (from
// `lower_terminal`) and matched with the `fancy-regex` oracle, so each mutant *is* a
// concrete buggy lowering — not a hand-faked number. The list is exactly the plan's:
// invert the guard set, off-by-one width, drop the EOF case, accept zero-width, and
// forget the guard entirely.

/// One deliberately-wrong trailing lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailMutation {
    /// The correct lowering (the control — must *not* diverge).
    None,
    /// Ignore the trailing guard entirely — take the body's greedy match.
    ForgetGuard,
    /// Flip the guard polarity (`(?!S)` ↔ `(?=S)`).
    InvertGuard,
    /// Wrongly accept a zero-width prefix.
    AcceptZeroWidth,
    /// Mishandle end-of-input: drop an otherwise-valid match that ends at EOF under a
    /// negative guard (as if "no next char" meant the guard could not be confirmed).
    DropEof,
    /// Check the guard one position too far (off-by-one window width).
    OffByOneWidth,
}

/// The names + variants of every trailing equivalence-layer mutant the meta-test runs
/// (excluding the `None` control).
pub fn trailing_mutants() -> Vec<(&'static str, TrailMutation)> {
    vec![
        ("forget-the-guard", TrailMutation::ForgetGuard),
        ("invert-the-guard-set", TrailMutation::InvertGuard),
        ("accept-zero-width", TrailMutation::AcceptZeroWidth),
        ("drop-the-eof-case", TrailMutation::DropEof),
        ("off-by-one-width", TrailMutation::OffByOneWidth),
    ]
}

/// A trailing terminal's branch decomposition, with the `fancy-regex` matchers it
/// needs **precompiled** (compiling per corpus input would make the meta-test O(many
/// thousands) of regex builds — pathologically slow).
struct CompiledBranch {
    /// `^(?:body)$` — tests whether a prefix is an *exact* body match.
    body_exact: fancy_regex::Regex,
    /// `(neg, \A(?:guard))` for a guarded branch.
    guard: Option<(bool, fancy_regex::Regex)>,
}

/// Compile the branch matchers for a trailing terminal once, from `lower_terminal`'s
/// own decomposition (so the mutant is a faithful corruption of the real lowering).
fn compile_branches(pattern: &str) -> Option<Vec<CompiledBranch>> {
    let branches = match lark_rs::lower_terminal("M", pattern) {
        Ok(lark_rs::Lowered::Trailing(b)) => b,
        _ => return None,
    };
    let mut out = Vec::new();
    for br in &branches {
        let body_exact = fancy_regex::Regex::new(&format!("^(?:{})$", br.body)).ok()?;
        let guard = match &br.guard {
            Some(g) => Some((
                g.neg,
                fancy_regex::Regex::new(&format!(r"\A(?:{})", g.guard)).ok()?,
            )),
            None => None,
        };
        out.push(CompiledBranch { body_exact, guard });
    }
    Some(out)
}

/// Byte lengths `l` (char boundaries, `0..=len`) at which `body_exact` matches
/// `input[..l]` exactly — the body's accept set.
fn body_accept_lengths(body_exact: &fancy_regex::Regex, input: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for l in 0..=input.len() {
        if input.is_char_boundary(l) && body_exact.is_match(&input[..l]).unwrap_or(false) {
            out.push(l);
        }
    }
    out
}

/// Whether the (precompiled) guard holds at byte offset `e`, under `mutation`.
fn guard_holds(
    neg: bool,
    guard_re: &fancy_regex::Regex,
    input: &str,
    e: usize,
    mutation: TrailMutation,
) -> bool {
    if mutation == TrailMutation::DropEof && e == input.len() && neg {
        return false; // EOF mishandling: drop the match that ends at end-of-input
    }
    let probe = match mutation {
        TrailMutation::OffByOneWidth => e + 1, // one position too far
        _ => e,
    };
    let matched = probe <= input.len()
        && input.is_char_boundary(probe)
        && guard_re
            .find(&input[probe..])
            .ok()
            .flatten()
            .is_some_and(|m| m.start() == 0);
    let neg = if mutation == TrailMutation::InvertGuard {
        !neg
    } else {
        neg
    };
    if neg {
        !matched
    } else {
        matched
    }
}

/// The match-prefix length (in **bytes**) a (possibly-mutated) trailing lowering
/// computes for `input`, or `None`. `TrailMutation::None` is the correct lowering.
fn mutant_match(
    branches: &[CompiledBranch],
    input: &str,
    mutation: TrailMutation,
) -> Option<usize> {
    // Ordered alternation: first branch that matches wins.
    for br in branches {
        let accepts = body_accept_lengths(&br.body_exact, input);
        let m = match &br.guard {
            None => accepts.iter().max().copied(),
            Some((neg, guard_re)) => match mutation {
                TrailMutation::ForgetGuard => accepts.iter().max().copied(),
                TrailMutation::AcceptZeroWidth => Some(0),
                _ => accepts
                    .iter()
                    .rev()
                    .copied()
                    .find(|&e| guard_holds(*neg, guard_re, input, e, mutation)),
            },
        };
        if let Some(end) = m {
            return Some(end);
        }
    }
    None
}

/// Whether `mutation` **survives** the trailing population's terminal-level
/// equivalence layer — i.e. it never diverges from `fancy-regex` over any generated
/// trailing terminal's exhaustive corpus. The correct lowering survives (returns
/// `true`); every real mutant must be *caught* (diverge somewhere → `false`), so a
/// surviving mutant is a hole in the net.
pub fn trailing_mutant_survives(mutation: TrailMutation) -> bool {
    for t in supported_terminals()
        .into_iter()
        .filter(|t| t.shape == ShapeClass::TrailingBoundary)
    {
        let (Some(oracle_re), Some(branches)) =
            (fancy_matcher(&t.pattern), compile_branches(&t.pattern))
        else {
            continue;
        };
        for input in corpus(&t.alphabet, t.max_len) {
            let oracle = fancy_prefix(&oracle_re, &input); // chars
            let mutant =
                mutant_match(&branches, &input, mutation).map(|end| input[..end].chars().count());
            if mutant != oracle {
                return false; // caught: diverges here
            }
        }
    }
    true // survived everywhere
}
