//! The four audited **delimited-token idiom** recognizers + lowerings
//! (`python.STRING`, `lark.REGEXP`, `python.LONG_STRING`, dotmotif `FLEXIBLE_KEY`).
//! The three string-family recognizers are kept deliberately **separate** and
//! non-parameterized — see the `do NOT unify` section comment below (architect
//! decision #478, 2026-06-30). Moved verbatim from the former single-file `lower.rs`
//! (issue #478 submodule split); behavior unchanged.

use super::*;

// ─── The string-literal opening-guard idiom (python.STRING family) ──────────────
//
// `python.STRING` is `([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?')`.
// Its `(?!"")` sits **after a variable-width prefix + the opening quote** — an
// internal/variable-position leading boundary the generic boundary path cannot lower
// (it is not at a fixed offset). `docs/LEXER_DFA_PLAN.md` calls for an NFA-state splice:
// "peek-branch states where the forbidden continuation ("" after the opening quote)
// leads to a DEAD (non-accepting) state." We realize that splice by case analysis,
// composing with the variable-width prefix, the lazy body, and the `(?<!\\)` lookbehind:
//
//   * **Lazy body + escape lookbehind → greedy character class.** The arm body
//     `.*?(?<!\\)(\\\\)*?<q>` is normalized *internally* (no grammar edit) to its proven
//     greedy equivalent `(?:[^<q>\\<nl>]|\\.)*<q>` — the Type-A rewrite
//     `tests/test_lookaround.rs::matchlen` (`string_lookaround_free_rewrite_is_not_equivalent`)
//     pins as match-length-identical to fancy **except** for the `(?!"")` divergence.
//     `<nl>` (the `\n` exclusion) is present iff the terminal is *not* DOTALL — under
//     DOTALL the body may span newlines, exactly as `LONG_STRING`'s `(?is)` body does.
//   * **The `(?!"")` splice.** Given that normalized body can never *begin* with the
//     delimiter (`[^<q>…]` excludes it, `\\.` starts with a backslash), the forbidden
//     continuation `<q><q>` right after the opening quote can only arise when the body is
//     **empty** — i.e. the token is the empty string `<q><q>` and the assertion's second
//     character lies *past* the matched token. So the splice reduces, exactly, to:
//       - a **non-empty** arm `<prefix><q>(?:[^<q>\\<nl>]|\\.)+<q>` — unguarded (the
//         `(?!"")` is vacuous, the body's first char is never the delimiter); and
//       - an **empty** arm `<prefix><q><q>` carrying a trailing guard `(?!<q>)` — the
//         empty string is valid only when the next input char is not another delimiter
//         (`""""` is a lex error; `"" ""` is two empty strings).
//     The two arms are mutually exclusive at any position (the char after the opening
//     quote is the delimiter in exactly one of them), so their relative priority never
//     bites. The empty arm's base `<prefix><q><q>` is *prefix-free* (the fixed `<q><q>`
//     pins the variable prefix's length), so the guarded longest-accept accumulator
//     reproduces fancy's match (see [`is_prefix_free`]).
//
// The recognizer matches **only** this exact shape; anything else returns `None` and the
// caller falls back to the generic boundary lowering (which rejects/declines it) — the
// reject-when-unsure direction. Newly-accepted instances are gated by the Route-1 proof
// (`tests/test_lowering_proof.rs`, the real nested STRING representative), the generative
// equivalence layer, and the python.lark differential.

/// A recognized string-literal opening-guard idiom: an optional bounded-width,
/// assertion-free prefix followed by an alternation of quote-delimited arms, each
/// `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>` for a single-character delimiter `<q>`.
pub struct StringIdiom {
    /// The prefix regex source (e.g. `([ubf]?r?|r[ubf])`), or empty when there is none.
    prefix: String,
    /// The delimiter source of each arm (e.g. `"` then `'`), in source order.
    delims: Vec<String>,
}

impl StringIdiom {
    /// Lower the idiom into its per-arm branches (two per arm: a non-empty plain branch
    /// and an empty trailing-guarded branch). `dotall` controls whether the body class
    /// admits a newline (excluded iff not DOTALL). Declines (the conservative direction)
    /// if an empty arm's base is not guard-realizable.
    pub(super) fn lower(
        &self,
        pattern: &str,
        dotall: bool,
    ) -> Result<Vec<LoweredBranch>, LowerDecline> {
        let nl = if dotall { "" } else { r"\n" };
        let mut branches = Vec::new();
        for d in &self.delims {
            // The delimiter is a fixed literal (the recognizer's `literal_delimiter_source`
            // guarantees a bare non-metacharacter or an escaped punctuation literal), so it
            // is safe both bare (the open/close `<q>`) and inside the negated class
            // `[^<q>\\<nl>]`.
            // Non-empty arm: unguarded greedy escaped body. The `(?!<q><q>)` is vacuous
            // here (the body never begins with the delimiter).
            let non_empty = format!("{p}{d}(?:[^{d}\\\\{nl}]|\\\\.)+{d}", p = self.prefix);
            branches.push(LoweredBranch {
                regex: non_empty,
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            });
            // Empty arm: `<prefix><q><q>` with a trailing `(?!<q>)` guard — the spliced
            // residual of `(?!"")` once the in-token part is shown vacuous.
            let empty = format!("{p}{d}{d}", p = self.prefix);
            if !is_guard_realizable(&empty, dotall) {
                return Err(decline(
                    pattern,
                    DeclineReason::EmptyArmNotRealizable,
                    "the empty-string arm's base is not guard-realizable (prefix not \
                     length-deterministic), so the trailing-guard accumulator cannot \
                     reproduce the original match",
                ));
            }
            branches.push(LoweredBranch {
                regex: empty,
                leading: None,
                trailing: Some(GuardSpec {
                    neg: true,
                    set: d.clone(),
                }),
                lookbehind: Vec::new(),
            });
        }
        Ok(branches)
    }
}

// ─── Why these three recognizers are kept separate (do NOT unify) ────────────
//
// `recognize_string_idiom`, `recognize_long_string_idiom`, and
// `recognize_short_string_idiom` (below) are near-duplicates, and it is tempting
// to fold them into one parameterized `recognize_delimited_idiom`. We
// deliberately do not. Each recognizer pins the *exact* bundled shape of its
// idiom, and that per-idiom matcher IS the soundness proof that its lowering
// reproduces the original match — the same "a variant must re-prove, not ride
// along" invariant the `regexp` recognizer documents further down. A shared
// abstraction trades that independent auditability for DRY: it makes it easy for
// a later edit to widen one idiom's accept set through the common helper, and the
// differential oracle does **not** catch that — a faithful unification and an
// accidental widening both stay green until a real grammar hits the gap. The ~3×
// duplication is the intended cost of keeping every idiom's soundness
// independently checkable. Architect decision (#478, 2026-06-30): keep separate;
// do not DRY this. (The orthogonal, no-fork half of #478 — splitting this file
// into submodules — stays available as good-autonomous work.)

/// Recognize the [`StringIdiom`] in a parsed terminal `node`, or `None`. Structural and
/// exact: the only newly-supported shape is `python.STRING`'s `(?!"")`-after-prefix
/// opening guard, so the matcher pins the precise arm shape and declines everything else.
pub fn recognize_string_idiom(node: &Node) -> Option<StringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(StringIdiom { prefix, delims })
}

/// Split `node` into `(prefix_source, arms_node)`: an optional leading bounded-width,
/// assertion-free prefix and the alternation-of-arms that follows it. The arms may sit
/// directly at top level, or (as in `python.STRING`) inside a single trailing group.
pub(super) fn split_prefix_and_arms(node: &Node) -> Option<(String, &Node)> {
    match node {
        // `PREFIX (arm|arm|…)` — the bundled shape: a concat of [prefix-group, arms-group].
        Node::Concat(parts) if parts.len() == 2 => {
            let prefix = &parts[0];
            if prefix.has_assertion() || width_range(prefix).1.is_none() {
                return None; // prefix must be assertion-free and bounded-width
            }
            let arms = unwrap_arms(&parts[1])?;
            Some((prefix.to_source(), arms))
        }
        // No prefix: the arms alternation (optionally wrapped in one group) at top level.
        other => unwrap_arms(other).map(|arms| (String::new(), arms)),
    }
}

/// Peel a single capturing/non-capturing group wrapper to reach the arms `Alt` (or a
/// bare single arm). Returns the inner node iff it is an `Alt` or a `Concat` (one arm).
///
/// **Only `(` and `(?:` opens are peeled — never a flag-scoped `(?i:`/`(?s:` wrapper.**
/// Peeling a flag wrapper would silently discard its flags: the lowering would emit a
/// branch whose body class reflects the *caller's* `dotall` while the original pattern
/// ran under the wrapper's — the exact dotall mis-lowering the
/// `g_regex_flags_dotall_long_string` seam fixture pins. The engine strips a
/// whole-pattern flag wrapper back into the flag bitset *before* routing
/// (`strip_whole_pattern_flag_wrapper` in `crate::lexer`), so a wrapper reaching here
/// is out-of-idiom and must decline (reject-when-unsure).
pub(super) fn unwrap_arms(node: &Node) -> Option<&Node> {
    match node {
        Node::Group { open, body, quant } if quant.is_empty() && (open == "(" || open == "(?:") => {
            match body.as_ref() {
                inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
                _ => None,
            }
        }
        inner @ (Node::Alt(_) | Node::Concat(_)) => Some(inner),
        _ => None,
    }
}

/// Match one arm `<q>(?!<q><q>).*?(?<!\\)(\\\\)*?<q>`, returning the delimiter source
/// `<q>`, or `None` if the arm is not exactly that shape.
pub(super) fn match_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 6 {
        return None;
    }
    let delim = literal_delimiter_source(&parts[0])?;

    // parts[1]: (?!<delim><delim>)
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Ahead,
            body,
            quant,
        } if quant.is_empty() && body.to_source() == format!("{delim}{delim}") => {}
        _ => return None,
    }

    // parts[2]: the lazy any-body `.*?`
    if !matches!(&parts[2], Node::Atom(s) if s == ".*?") {
        return None;
    }

    // parts[3]: (?<!\\)
    match &parts[3] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && body.to_source() == r"\\" => {}
        _ => return None,
    }

    // parts[4]: (\\\\)*? — the even-backslash run
    match &parts[4] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[5]: the closing delimiter, identical to the opening one.
    if literal_delimiter_source(&parts[5])? != delim {
        return None;
    }
    Some(delim)
}

/// The source of a single-character **literal** delimiter — the only delimiters the
/// idiom lowering can faithfully reproduce, because the delimiter is emitted in the
/// lowered base both *bare* (the open/close `<q>`) and *inside a negated class*
/// (`[^<q>\\…]`), and must denote exactly one fixed character in both positions:
///
///   * a **bare ordinary literal** — any char that is not a regex metacharacter or a
///     character-class-special char (so `"`, `'`, `/`, `:`, … are fine; `.`, `^`, `$`,
///     `*`, `+`, `?`, `(`, `)`, `[`, `]`, `{`, `}`, `|`, `\`, `-` are not); or
///   * an **escaped literal** `\X` where `X` is ASCII *punctuation* (`\.`, `\"`, `\/`,
///     `\$`, … — a literal-escape of a metacharacter or other punctuation, emitted
///     escaped in both positions so it stays literal).
///
/// Returns `None` for everything else — crucially `.` (any char), the anchors
/// (`^ $ \b \B \A \z \Z \G`), and the class escapes (`\d \w \s …`): these are *not*
/// fixed single literals, so an arm built on them would mis-lower. Declining them routes
/// the terminal to `fancy-regex` (reject-when-unsure) and closes the false-accept.
pub(super) fn literal_delimiter_source(node: &Node) -> Option<String> {
    let s = match node {
        Node::Atom(s) => s.as_str(),
        _ => return None,
    };
    let chars: Vec<char> = s.chars().collect();
    match chars.as_slice() {
        // A bare ordinary literal: not a regex metacharacter, not class-special.
        [c] if is_plain_literal(*c) => Some(c.to_string()),
        // An escaped punctuation literal (`\.`, `\"`, `\/`, …); excludes `\d \w \b \n …`
        // (letters/digits — classes, assertions, encoded literals).
        ['\\', c] if c.is_ascii_punctuation() => Some(format!("\\{c}")),
        _ => None,
    }
}

/// Whether `c` is an ordinary literal usable *bare* as a delimiter — neither a regex
/// metacharacter (special standalone) nor a character-class-special char (`-` `]` `^`
/// `\`). Anything excluded here can still be a delimiter in its **escaped** form.
pub(super) fn is_plain_literal(c: char) -> bool {
    !matches!(
        c,
        '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '-'
    )
}

// ─── The regex-literal idiom (the bundled lark.REGEXP, Stage B) ──────────────────
//
// `lark.REGEXP` is `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*` — a `/ body / flags`
// delimited token whose `(?!\/)` sits *between* the opening slash and the lazy body, an
// internal position the top-level classifier rejects. It is the second audited
// **delimited-token idiom** (`docs/LEXER_DFA_PLAN.md`, Stage B), after the M4 STRING
// splice. The lowering rests on one exact observation:
//
//   **The guard reduces to "the body is non-empty."** At the guard position (right
//   after the opening `\/`), the forbidden continuation is a `/`. Every body
//   alternative starts with a char that is *not* `/` (`\\\/` and `\\\\` start with a
//   backslash; `[^\/]` excludes the slash), and the close `\/` starts with exactly `/`.
//   So at that position the engine can close (next char is `/`) **xor** consume a body
//   item — never both. `(?!\/)` therefore fails exactly when the body would match zero
//   items and the close would fire immediately (the empty `//`), and holds in every
//   other case where the token can proceed. Dropping the guard and bumping the lazy
//   repetition's minimum — `(…)*?` → `(…)+?` — is an *exact* rewrite, not an
//   approximation. (The same close-vs-item first-char disjointness holds at **every**
//   iteration boundary, which is also why `tests/test_lookaround.rs::matchlen`'s E2a
//   harness found this terminal Type-A regex-rewritable.)
//
// The single lowered branch is **unguarded** and joins the leftmost-first plain
// engine, which reproduces the lazy `+?` / ordered-alternation match end exactly
// (including the backtracking "dangling escaped slash" close — `/a\/b` matches
// `/a\/` — and the greedy `[imslux]*` flags suffix), so no guard machinery and no
// realizability question is involved. Gated by the route pins
// (`tests/test_lowering_routes.rs`), the hand canaries (`tests/test_regexp_splice.rs`),
// the generative equivalence + `*?`-mutant (`tests/test_lowering_equivalence.rs`), the
// state-pruned Route-1 proof (`tests/test_lowering_proof.rs`), and the scanner
// differential population.
//
// The recognizer matches **only** the exact bundled shape — anything else returns
// `None` and falls through to the generic path (which rejects/declines it), the
// reject-when-unsure direction. It is deliberately *not* parameterized over the
// delimiter, the body alternatives, their order, the quantifier, or the flags suffix:
// each of those is load-bearing in the reduction above (the first-char disjointness,
// the close shape, the laziness), so a variant must re-prove, not ride along.

/// A recognized regex-literal idiom — exactly the bundled `lark.REGEXP` shape
/// `\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*`. Carries no parameters: the recognizer
/// pins every part of the shape, so the lowering is a fixed, audited rewrite.
pub struct RegexpIdiom;

/// Recognize the [`RegexpIdiom`] in a parsed terminal `node`, or `None`. Structural and
/// exact — see the section comment above for why no variant is admitted.
pub fn recognize_regexp_idiom(node: &Node) -> Option<RegexpIdiom> {
    let parts = match node {
        Node::Concat(parts) if parts.len() == 4 => parts,
        _ => return None,
    };
    // parts[0]: the opening delimiter, exactly the escaped slash `\/`.
    if !matches!(&parts[0], Node::Atom(s) if s == r"\/") {
        return None;
    }
    // parts[1]: the empty-body guard, exactly `(?!\/)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Ahead,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\/") => {}
        _ => return None,
    }
    // parts[2]: the lazy escaped body, exactly `(\\\/|\\\\|[^\/])*?` — the capturing
    // group, the three alternatives in source order, and the lazy star are all pinned.
    match &parts[2] {
        Node::Group { open, body, quant } if open == "(" && quant == "*?" => {
            let arms = match body.as_ref() {
                Node::Alt(arms) if arms.len() == 3 => arms,
                _ => return None,
            };
            for (arm, want) in arms.iter().zip([r"\\\/", r"\\\\", r"[^\/]"]) {
                if !matches!(arm, Node::Atom(s) if s == want) {
                    return None;
                }
            }
        }
        _ => return None,
    }
    // parts[3]: the close + flags tail, exactly `\/[imslux]*`.
    if !matches!(&parts[3], Node::Atom(s) if s == r"\/[imslux]*") {
        return None;
    }
    Some(RegexpIdiom)
}

impl RegexpIdiom {
    /// Lower the idiom: drop the `(?!\/)` and bump the lazy body to non-empty
    /// (`*?` → `+?`) — the exact rewrite the section comment proves. One unguarded,
    /// lookaround-free branch; its lazy/priority match end is the plain leftmost-first
    /// engine's native semantics.
    pub(super) fn lower(&self) -> Vec<LoweredBranch> {
        vec![LoweredBranch {
            regex: r"\/(\\\/|\\\\|[^\/])+?\/[imslux]*".to_string(),
            leading: None,
            trailing: None,
            lookbehind: Vec::new(),
        }]
    }
}

// ─── The long-string idiom (the bundled python.LONG_STRING, Stage B) ─────────────
//
// `python.LONG_STRING` is `([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?''')`
// with `/is` flags — a `<prefix> <qqq> body <qqq>` delimited token whose `(?<!\\)`
// lookbehind sits after the variable-width `.*?`, the no-fixed-offset position the
// generic M3 path declines. It is the third audited **delimited-token idiom**
// (`docs/LEXER_DFA_PLAN.md`, Stage B), after the M4 STRING splice and the REGEXP
// regex-literal idiom. The lowering is the escape-pair body normalization:
//
//   **The `(?<!\\)(\\\\)*?` is absorbed by forced escape pairing.** Rewrite the lazy
//   escaped body to lazy escape-pair items:
//
//       .*?(?<!\\)(\\\\)*?<qqq>   →   (?:[^\\<nl>]|\\.)*?<qqq>
//
//   (`<nl>` = `\n` iff the terminal is not DOTALL, exactly the string idiom's
//   threading.) A backslash can only be consumed as the start of a `\\.` pair (the
//   class excludes it), so item segmentation is forced and an item *boundary* exists
//   exactly at the positions where the maximal preceding backslash run has even
//   length — which is precisely the `(?<!\\)(\\\\)*?` close condition. The lazy `*?`
//   is **kept**: both sides close at the *first* even-parity `<qqq>`. This is the
//   committed Type-A finding `tests/test_lookaround.rs::long_string_match_length_equivalence`
//   pins (`LONG_ORIG ≡ LONG_NEW` over an exhaustive corpus with quotes, backslashes,
//   newlines, and the `r` prefix). Unlike the STRING splice, the delimiter quote is
//   *not* excluded from the body class — a lone `"` (or `""`) inside the body does not
//   close; laziness picks the first full `<qqq>`, so no multi-char delimiter automaton
//   is needed.
//
// The per-arm branches are **unguarded** (prefix duplicated per branch, arms in source
// order) and join the leftmost-first plain engine, whose native lazy/priority semantics
// reproduce the match end — the REGEXP precedent, so no guard machinery and no
// realizability question. The per-arm split is itself verified: leftmost-first across
// the two prefix-duplicated branches ≡ the original single pattern under `(?is)`
// (0 divergences over 2,015,539 inputs, lengths 0–8 over `" ' \ a \n r`), and the
// non-DOTALL `[^\\\n]` variant ≡ the unflagged original (0 divergences over 349,525
// inputs). Gated by the route pins (`tests/test_lowering_routes.rs`), the hand canaries
// (`tests/test_long_string_splice.rs`), the generative equivalence + parity/two-quote/
// greedy mutants (`tests/test_lowering_equivalence.rs`), the state-pruned Route-1 proof
// (`tests/test_lowering_proof.rs`), and the scanner-differential population.
//
// The recognizer matches **only** the exact bundled arm shape — delimiters `"""` or
// `'''` only, open == close, the lazy `.*?`, the `(?<!\\)` lookbehind, and the lazy
// `(\\\\)*?` escape group are all pinned; the optional prefix rides the same
// [`split_prefix_and_arms`] gate the string idiom uses (bounded, assertion-free).
// Anything else returns `None` and falls through to the generic path (which declines
// the variable-offset lookbehind), the reject-when-unsure direction.

/// A recognized long-string idiom: an optional bounded-width, assertion-free prefix
/// followed by 1..n arms, each exactly `<qqq>.*?(?<!\\)(\\\\)*?<qqq>` for a
/// triple-quote delimiter `<qqq>` ∈ {`"""`, `'''`}.
pub struct LongStringIdiom {
    /// The prefix regex source (e.g. `([ubf]?r?|r[ubf])`), or empty when there is none.
    prefix: String,
    /// The triple-quote delimiter of each arm (`"""` / `'''`), in source order.
    delims: Vec<String>,
}

/// Recognize the [`LongStringIdiom`] in a parsed terminal `node`, or `None`. Structural
/// and exact — see the section comment above for why no variant is admitted.
pub fn recognize_long_string_idiom(node: &Node) -> Option<LongStringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_long_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(LongStringIdiom { prefix, delims })
}

/// Match one arm `<qqq>.*?(?<!\\)(\\\\)*?<qqq>`, returning the triple-quote delimiter
/// `<qqq>`, or `None` if the arm is not exactly that shape. The opening delimiter and
/// the lazy `.*?` arrive merged in a single atom (no structural boundary between them).
pub(super) fn match_long_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 4 {
        return None;
    }

    // parts[0]: `<qqq>.*?` — the opening triple quote + the lazy any-body, one atom.
    // Only the two bundled delimiters are admitted.
    let delim = match &parts[0] {
        Node::Atom(s) if s == "\"\"\".*?" => "\"\"\"".to_string(),
        Node::Atom(s) if s == "'''.*?" => "'''".to_string(),
        _ => return None,
    };

    // parts[1]: `(?<!\\)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\\") => {}
        _ => return None,
    }

    // parts[2]: `(\\\\)*?` — the lazy even-backslash run.
    match &parts[2] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[3]: the closing delimiter, identical to the opening one.
    if !matches!(&parts[3], Node::Atom(s) if *s == delim) {
        return None;
    }
    Some(delim)
}

impl LongStringIdiom {
    /// Lower the idiom: normalize each arm's lazy escaped body to lazy escape-pair
    /// items (absorbing the `(?<!\\)(\\\\)*?` — the exact rewrite the section comment
    /// proves), keeping the lazy close. One unguarded branch per arm, prefix duplicated;
    /// `dotall` controls whether the body class admits a newline (excluded iff not
    /// DOTALL, so the class tracks what the original `.` matches under the terminal's
    /// flags; the `\\.` pair's second char tracks it natively via the engine's flag
    /// wrap).
    pub(super) fn lower(&self, dotall: bool) -> Vec<LoweredBranch> {
        let nl = if dotall { "" } else { r"\n" };
        self.delims
            .iter()
            .map(|d| LoweredBranch {
                regex: format!("{p}{d}(?:[^\\\\{nl}]|\\\\.)*?{d}", p = self.prefix),
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            })
            .collect()
    }
}

// ─── The short-string idiom (the wild-bank dotmotif FLEXIBLE_KEY, idiom #4) ──────
//
// dotmotif's `FLEXIBLE_KEY` is `".+?(?<!\\)(\\\\)*?"|'.+?(?<!\\)(\\\\)*?'` — a
// quote-delimited token with a **non-empty** lazy escaped body, whose `(?<!\\)`
// lookbehind sits after the variable-width `.+?` (the no-fixed-offset position the
// generic M3 path declines). It is the fourth audited **delimited-token idiom**, the
// guardless single-delimiter sibling of the M4 STRING splice (the same `<q> body <q>`
// family; LONG_STRING is the triple-quote sibling). The lowering is the same
// escape-pair body normalization, with one twist the missing `(?!<q><q>)` guard forces:
//
//   **A close needs more body than its own escape run.** The body decomposes as
//   `X·P`: `X` = the `.+?` chars (**≥ 1**, anything), `P` = the `(\\\\)*?` even
//   backslash run, with `(?<!\\)` forcing `P` to cover the *entire* maximal trailing
//   backslash run. So a `<q>` at body length `ℓ` with a maximal trailing backslash
//   run of length `r` closes iff `r` is even **and `ℓ > r`** — and the lazy close
//   fires at the *first* such `<q>`. A `<q>` where that fails is **consumed as a
//   body char**: at `ℓ = r = 0` (`"""` is one 3-char token, the empty `""` is no
//   token) and, the subtle case, after a **pure-pair body** (`ℓ = r > 0`: `"\\"` is
//   no token — `X` would be empty — and `"\\""` is one 5-char token whose third
//   quote is body). The exact lookaround-free equivalent tracks "body so far is pure
//   backslash pairs" structurally:
//
//       <q>.+?(?<!\\)(\\\\)*?<q>
//         →   <q> (?:\\\\)* (?:[^\\<nl>]|\\[^\\<nl>]) (?:[^<q>\\<nl>]|\\.)* <q>
//
//   — a greedy pure-pair run (the `ℓ = r` zone, where a `<q>` is consumed, never a
//   close), then one **mandatory transition item** (any non-backslash char,
//   *including a bare `<q>`*, or an escape pair whose second char is not a
//   backslash — exactly the moves that make `ℓ > r` and keep it so), then
//   LONG_STRING's escape-pair items with the delimiter excluded so the greedy `*`
//   closes at the first free-standing `<q>` (the M4 close-exclusion argument; at
//   every item boundary past the transition the trailing run is even and `ℓ > r`,
//   so the first free `<q>` is exactly the original's lazy close). The pure-pair
//   run and the transition pair are first-two-char disjoint, so the decomposition
//   is deterministic and the lowered branch is unguarded — its leftmost-first match
//   end is the plain engine's native semantics. `<nl>` (the `\n` exclusion, in the
//   classes and the pair tails) is present iff the terminal is not DOTALL, exactly
//   the STRING/LONG_STRING threading.
//
// Gated by the recognizer-exactness + behavior unit tests below, the generative
// equivalence sweep vs the `fancy-regex` dev-oracle
// (`tests/test_lowering_equivalence.rs`), and end-to-end by the wild bank's dotmotif
// replay (23 real queries vs the Python-Lark oracle). The recognizer matches **only**
// this exact shape — in particular the non-empty `.+?`: the *empty-capable* `.*?`
// variant without a `(?!<q><q>)` guard closes at width 0 on `""` where this rewrite
// would consume a char, so it must keep declining (reject-when-unsure) until someone
// proves its own rewrite.

/// A recognized short-string idiom: an optional bounded-width, assertion-free prefix
/// followed by 1..n arms, each exactly `<q>.+?(?<!\\)(\\\\)*?<q>` for a
/// single-character literal delimiter `<q>`.
pub struct ShortStringIdiom {
    /// The prefix regex source, or empty when there is none.
    prefix: String,
    /// The delimiter source of each arm (e.g. `"` then `'`), in source order.
    delims: Vec<String>,
}

/// Recognize the [`ShortStringIdiom`] in a parsed terminal `node`, or `None`.
/// Structural and exact — see the section comment above for why no variant is
/// admitted.
pub fn recognize_short_string_idiom(node: &Node) -> Option<ShortStringIdiom> {
    let (prefix, arms_node) = split_prefix_and_arms(node)?;
    let arm_nodes: Vec<&Node> = match arms_node {
        Node::Alt(branches) => branches.iter().collect(),
        other => vec![other],
    };
    let mut delims = Vec::new();
    for arm in arm_nodes {
        delims.push(match_short_string_arm(arm)?);
    }
    if delims.is_empty() {
        return None;
    }
    Some(ShortStringIdiom { prefix, delims })
}

/// Match one arm `<q>.+?(?<!\\)(\\\\)*?<q>`, returning the delimiter source `<q>`, or
/// `None` if the arm is not exactly that shape. The opening delimiter and the lazy
/// non-empty body arrive merged in a single atom (no structural boundary between
/// them), like the long-string arm's.
pub(super) fn match_short_string_arm(arm: &Node) -> Option<String> {
    let parts = match arm {
        Node::Concat(parts) => parts.as_slice(),
        _ => return None,
    };
    if parts.len() != 4 {
        return None;
    }

    // parts[0]: `<q>.+?` — the opening delimiter + the lazy *non-empty* any-body, one
    // atom. The delimiter must be a single-character literal (the same contract as the
    // STRING idiom's `literal_delimiter_source`, for the same reason: it is emitted
    // both bare and inside a negated class below).
    let delim = match &parts[0] {
        Node::Atom(s) => {
            let head = s.strip_suffix(".+?")?;
            let head_node = Node::Atom(head.to_string());
            literal_delimiter_source(&head_node)?
        }
        _ => return None,
    };

    // parts[1]: `(?<!\\)`, unquantified.
    match &parts[1] {
        Node::Assertion {
            neg: true,
            look: Look::Behind,
            body,
            quant,
        } if quant.is_empty() && matches!(body.as_ref(), Node::Atom(s) if s == r"\\") => {}
        _ => return None,
    }

    // parts[2]: `(\\\\)*?` — the lazy even-backslash run.
    match &parts[2] {
        Node::Group { open, body, quant }
            if open == "("
                && quant == "*?"
                && matches!(body.as_ref(), Node::Atom(s) if s == r"\\\\") => {}
        _ => return None,
    }

    // parts[3]: the closing delimiter, identical to the opening one.
    if !matches!(&parts[3], Node::Atom(s) if *s == delim) {
        return None;
    }
    Some(delim)
}

impl ShortStringIdiom {
    /// Lower the idiom: one unguarded branch per arm — the exact rewrite the section
    /// comment proves (pure-pair run, mandatory transition item, close-excluded
    /// items). `dotall` controls whether the body classes admit a newline.
    pub(super) fn lower(&self, dotall: bool) -> Vec<LoweredBranch> {
        let nl = if dotall { "" } else { r"\n" };
        self.delims
            .iter()
            .map(|d| LoweredBranch {
                regex: format!(
                    "{p}{d}(?:\\\\\\\\)*(?:[^\\\\{nl}]|\\\\[^\\\\{nl}])(?:[^{d}\\\\{nl}]|\\\\.)*{d}",
                    p = self.prefix
                ),
                leading: None,
                trailing: None,
                lookbehind: Vec::new(),
            })
            .collect()
    }
}
