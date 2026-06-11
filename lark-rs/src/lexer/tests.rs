//! DfaScanner ≡ Scanner: focused parity unit tests (L1).
//!
//! The L0 differential oracle (tests/test_scanner_differential.rs) is the broad
//! contract. These pin the load-bearing edge cases directly, in-crate, so a
//! regression localizes to `match_at` without a corpus run — chiefly the
//! multi-pattern leftmost-first **tie-break** the plan flags as the one real risk.

use super::dfa::DfaScanner;
use super::pattern::strip_whole_pattern_flag_wrapper;
use super::scanner::Scanner;
use crate::error::GrammarError;
use crate::grammar::intern::SymbolId;
use crate::grammar::terminal::{Pattern, PatternRe, PatternStr, TerminalDef};

fn re_term(id: u32, name: &str, pat: &str, prio: i32) -> (SymbolId, TerminalDef) {
    let p = Pattern::Re(PatternRe::new(pat, 0).unwrap());
    (SymbolId(id), TerminalDef::new(name, p, prio))
}
fn str_term(id: u32, name: &str, val: &str, prio: i32) -> (SymbolId, TerminalDef) {
    let p = Pattern::Str(PatternStr::new(val));
    (
        SymbolId(id),
        TerminalDef::new(name, p, prio).with_string_type(true),
    )
}

fn both(terms: &[(SymbolId, TerminalDef)]) -> (Scanner, DfaScanner) {
    let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
    (
        Scanner::build(&refs, 0).unwrap(),
        DfaScanner::build(&refs, 0).unwrap(),
    )
}

/// Assert the two engines pick the byte-identical `(id, value)` at **every**
/// position of each input — the L1 contract, in miniature.
fn assert_agree(terms: &[(SymbolId, TerminalDef)], inputs: &[&str]) {
    let (s, d) = both(terms);
    for inp in inputs {
        for pos in 0..=inp.len() {
            assert_eq!(
                s.match_at(inp, pos),
                d.match_at(inp, pos),
                "engines diverged on {inp:?} at pos {pos}"
            );
        }
    }
}

/// Assert `DfaScanner::build` refuses `terms` with the expected categorized scope
/// error (`docs/LOOKAROUND_SCOPE.md`) — the L4 contract: no fallback engine, a
/// clean typed refusal instead.
fn assert_dfa_scope_error(
    terms: &[(SymbolId, TerminalDef)],
    scope: crate::lookaround::classify::Scope,
    issue: crate::lookaround::classify::LookaroundIssue,
) {
    let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
    match DfaScanner::build(&refs, 0) {
        Err(GrammarError::LookaroundScope {
            scope: got_scope,
            issue: got_issue,
            ..
        }) => {
            assert_eq!(got_scope, scope);
            assert_eq!(got_issue, issue);
        }
        Err(other) => panic!("expected a LookaroundScope error, got {other:?}"),
        Ok(_) => panic!("expected the build to refuse, but it succeeded"),
    }
}

#[test]
fn dfa_tiebreak_same_start_picks_lowest_rank_not_longest() {
    // Two regex terminals matching at the same start with different lengths.
    // `sort_terminals` orders by (priority, max_width, pattern-len, name); both
    // are unbounded-width regexes of the same priority, so the longer *source*
    // (`abc`) ranks first. Leftmost-first then takes that branch's own greedy
    // length — and crucially, where only the shorter (`ab`) ranks first, it must
    // win with length 2 even though `abc` would match longer. Both engines agree.
    assert_agree(
        &[re_term(1, "AB", "ab", 0), re_term(2, "ABC", "abc", 0)],
        &["abc", "ab", "abz", "a", "abcd", "x", ""],
    );
    // The decisive direction: make the *shorter* pattern rank first by source
    // length (`a.` is 2 chars, `abc` is 3 → `abc` first; use `ab?` vs `abcd`).
    let (_, d) = both(&[re_term(1, "SHORT", "ab", 5), re_term(2, "LONG", "abcd", 0)]);
    // SHORT has higher priority, so it ranks first and wins at "abcd" with len 2,
    // NOT the longest match (len 4). This is the Python-re leftmost-first tie-break.
    assert_eq!(d.match_at("abcd", 0), Some((SymbolId(1), "ab")));
}

#[test]
fn dfa_keyword_unless_retype_matches_regex_scanner() {
    let terms = [str_term(1, "IF", "if", 0), re_term(2, "NAME", "[a-z]+", 0)];
    assert_agree(&terms, &["if", "iffy", "if x", "i", "z", "if2"]);
    // Pin the engine-independent outcome too: the keyword retypes to IF (id 1),
    // a longer identifier stays NAME (id 2).
    let (_, d) = both(&terms);
    assert_eq!(d.match_at("if", 0), Some((SymbolId(1), "if")));
    assert_eq!(d.match_at("iffy", 0), Some((SymbolId(2), "iffy")));
}

#[test]
fn dfa_priority_and_width_ordering_matches_regex_scanner() {
    // OCT (priority 2) must beat INT at "0o777"; agreement across the boundary
    // and over a punctuation terminal that shares no start byte.
    assert_agree(
        &[
            re_term(1, "OCT", "0[oO][0-7]+", 2),
            re_term(2, "INT", "[0-9]+", 0),
            str_term(3, "PLUS", "+", 0),
        ],
        &["0o777", "0777", "123", "0", "+", "0o", "12+34", "0o+1"],
    );
}

#[test]
fn dfa_start_byte_prefilter_never_hides_a_match() {
    // Scan every position of a mixed string: the start-byte prefilter must skip
    // the engine only where no terminal could match, never where one does.
    assert_agree(
        &[
            re_term(1, "WORD", "[a-z]+", 0),
            re_term(2, "NUM", "[0-9]+", 0),
        ],
        &["abc123 def", "   x", "9z9z", "...."],
    );
}

/// `unless` keyword retyping over a LOWERED terminal: the keyword's full-match
/// test runs on the lowered branches + guards (`compute_unless`'s `FullMatcher`),
/// not on any fallback engine. `T=/ab(?!c)|q/` overlaps the keyword `"ab"` (its
/// trailing guard at the end of the value sees EOI → `(?!c)` holds, exactly as
/// the historical `^(?:…)$` full-match saw it), so `"ab"` retypes to `K`; the
/// guard still bites at scan time (`"abc"` is no `T` at 0).
#[test]
fn dfa_unless_retype_works_over_lowered_terminal() {
    let terms = [re_term(1, "T", "ab(?!c)|q", 0), str_term(2, "K", "ab", 0)];
    let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
    let d = DfaScanner::build(&refs, 0).expect("lowered terminal + keyword builds");
    assert_eq!(
        d.match_at("ab", 0),
        Some((SymbolId(2), "ab")),
        "retyped to K"
    );
    assert_eq!(d.match_at("q", 0), Some((SymbolId(1), "q")), "stays T");
    assert_eq!(d.match_at("abc", 0), None, "the trailing guard still bites");
}

#[test]
fn dfa_guarded_order_sensitive_base_is_a_categorized_nyi_error() {
    // A trailing guard over a base whose internal alternation is order-sensitive
    // (`(ab|abc)`) is NOT greedy-monotone: "longest accept where the guard holds"
    // would pick "abc" where leftmost-first wants "ab". `is_greedy_monotone` keeps
    // it off the accumulator; since L4 there is no fallback engine, so the build
    // refuses with the categorized NotYetImplemented error — never a mis-lowering.
    use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
    let terms = [re_term(1, "T", "(ab|abc)(?!z)", 0)];
    assert_dfa_scope_error(
        &terms,
        Scope::NotYetImplemented,
        LookaroundIssue::Declined(DeclineReason::NonRealizableGuardedBase),
    );
}

#[test]
fn dfa_sibling_guard_does_not_demote_plain_alternation() {
    // Regression for the cross-terminal selection bug: a guarded terminal in the
    // same scanner as an *unguarded* order-sensitive alternation must NOT flip the
    // plain terminal from leftmost-first to longest-match. `AB=/ab|abc/` (plain)
    // stays leftmost-first ("ab") even though `B=/x(?!y)/` is guarded.
    let terms = [re_term(1, "AB", "ab|abc", 0), re_term(2, "B", "x(?!y)", 0)];
    let (s, d) = both(&terms);
    assert_eq!(d.match_at("abc", 0), Some((SymbolId(1), "ab")));
    assert_agree(&terms, &["abc", "ab", "x", "xy", "abx", "xab"]);
    let _ = s;
}

#[test]
fn dfa_lazy_guarded_base_is_a_categorized_nyi_error() {
    // Regression for the lazy-body bug: a lazy quantifier in a guarded base
    // (`ab??(?!c)`) is not greedy-monotone — the longest-accept accumulator would
    // pick "ab" where leftmost-first (lazy) wants "a". The lowering declines it;
    // since L4 the build refuses with the categorized NotYetImplemented error.
    use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
    let terms = [re_term(1, "T", "ab??(?!c)", 0)];
    assert_dfa_scope_error(
        &terms,
        Scope::NotYetImplemented,
        LookaroundIssue::Declined(DeclineReason::NonRealizableGuardedBase),
    );
}

/// **The engine-path pin for the bundled idioms.** The grammar loader delivers a
/// terminal's `/…/is`-style flags **baked into the pattern** as one flag-scoped
/// wrapper (`(?is:…)`, `PatternRe.flags = 0`) — exactly what `re_term` models here.
/// `DfaScanner::build` must strip that wrapper back into the flag bitset
/// (`strip_whole_pattern_flag_wrapper`) so the bundled `python.STRING` /
/// `python.LONG_STRING` / `lark.REGEXP` idioms genuinely lower **on the engine
/// path**: the built scanner has NO fancy side-probe at all. (Before the strip,
/// the wrapped STRING silently rode the `Unsupported` compatibility fallback and
/// the wrapped LONG_STRING the decline route — invisible to the differential,
/// which the fancy reference backend matched anyway.) Behaviour is then pinned
/// against `Scanner` on flag-sensitive inputs: a multi-line docstring (DOTALL)
/// and case-folded prefixes (IGNORECASE).
#[test]
fn dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe() {
    let terms = [
        re_term(
            1,
            "STRING",
            r#"(?i:([ubf]?r?|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"|'(?!'').*?(?<!\\)(\\\\)*?'))"#,
            0,
        ),
        re_term(
            2,
            "LONG_STRING",
            r#"(?is:([ubf]?r?|r[ubf])(""".*?(?<!\\)(\\\\)*?"""|'''.*?(?<!\\)(\\\\)*?'''))"#,
            1,
        ),
        re_term(3, "REGEXP", r"\/(?!\/)(\\\/|\\\\|[^\/])*?\/[imslux]*", 0),
    ];
    let (_, d) = both(&terms);
    // Since L4 there is no fallback engine at all, so the *build succeeding* is
    // itself the pin (a refused terminal is a categorized build error); the
    // structural assert below shows the idioms genuinely populate the engines.
    assert!(
        d.plain.is_some() && d.guarded.is_some(),
        "the lowered idioms populate both engines (unguarded branches + STRING's \
         guarded empty arm)"
    );
    assert_agree(
        &terms,
        &[
            "\"\"\"a\nb\"\"\"",  // DOTALL: the docstring spans lines
            "R\"x\"",            // IGNORECASE: case-folded prefix (STRING)
            "RB\"\"\"x\n\"\"\"", // IGNORECASE+DOTALL prefix (LONG_STRING)
            "\"\"\"\"",          // the (?!"") canary: no STRING opens in the run
            "\"\"\"\"\"\"",      // six quotes: one empty LONG_STRING
            "/a\\/b/i",          // REGEXP with escaped slash + flag
            "\"a\" '''b'''",     // STRING then LONG_STRING
        ],
    );
}

/// **The model-vs-reality closure for the zero-probe pin.** The test above models
/// the loader's flag-bake format by hand (`re_term` with `(?is:…)`-wrapped
/// patterns); if `Pattern::to_inline_regex` ever changed its emitted form, that
/// model could keep passing while the *real* import path silently regressed to the
/// fancy probe — exactly the invisible rot this PR dug `python.STRING` out of. So
/// this twin builds the scanner from the **real loader output**: a grammar that
/// `%import`s all three bundled lookaround terminals, run through `load_grammar` →
/// `lower` → `basic_lexer_conf`, must also build — and since L4 a successful build
/// IS the zero-probe claim (a refused terminal is a categorized build error; no
/// fallback engine exists).
#[test]
fn dfa_real_loader_bundled_imports_have_no_fancy_probe() {
    let grammar = "start: STRING | LONG_STRING | REGEXP\n\
                   %import python.STRING\n\
                   %import python.LONG_STRING\n\
                   %import lark.REGEXP\n";
    let g = crate::load_grammar(grammar, &["start".to_string()], false, false)
        .expect("grammar importing the three bundled lookaround terminals builds");
    let cg = crate::lower(&g);
    let conf = crate::basic_lexer_conf(&cg, 0);
    let refs: Vec<(SymbolId, &TerminalDef)> = conf.terminals.iter().map(|(i, t)| (*i, t)).collect();
    let d = DfaScanner::build(&refs, conf.global_flags).expect(
        "the REAL loader-imported bundled terminals must lower — a refusal here \
         means `to_inline_regex`'s bake format and the flag-wrapper strip drifted",
    );
    assert!(d.plain.is_some() && d.guarded.is_some());
}

/// **The VERBOSE conservatism pin.** `strip_whole_pattern_flag_wrapper` must NOT
/// strip a `(?x:…)` wrapper: the lookaround parser's width/offset analysis is not
/// verbose-aware, so a stripped `x`-body would count whitespace as literal width
/// while the re-wrapped branch ignores it — a fixed-offset lookbehind could lower
/// with a wrong offset (a false-accept). An `x`-wrapped lookaround terminal must
/// never lower: the strip leaves the wrapper alone, and since L4 the build refuses
/// it with the honest categorized NotYetImplemented `VerboseMode` error (not the
/// classifier's mislabel of the group-nested assertion as out-of-scope internal
/// lookahead).
#[test]
fn verbose_flag_wrapper_is_not_stripped_into_lowering() {
    use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
    // Whitespace inside the verbose body is regex-insignificant at runtime but
    // would be width-significant to a naive strip + reparse.
    let terms = [re_term(1, "VX", r"(?x:[0-9]+ (?![0-9]))", 0)];
    assert_dfa_scope_error(
        &terms,
        Scope::NotYetImplemented,
        LookaroundIssue::Declined(DeclineReason::VerboseMode),
    );
    // The helper itself: an `x` anywhere in the wrapper letters refuses the strip
    // wholesale; the plain `i`/`s` strips still work.
    assert_eq!(
        strip_whole_pattern_flag_wrapper("(?x:a b)", 0),
        ("(?x:a b)".to_string(), 0)
    );
    assert_eq!(
        strip_whole_pattern_flag_wrapper("(?isx:a)", 0),
        ("(?isx:a)".to_string(), 0)
    );
    let f = crate::grammar::terminal::flags::IGNORECASE | crate::grammar::terminal::flags::DOTALL;
    assert_eq!(
        strip_whole_pattern_flag_wrapper("(?is:a)", 0),
        ("a".to_string(), f)
    );
}

/// **The global-VERBOSE conservatism pin** (PR #137 review, blocker 1). The
/// verbose false-accept hazard is not only the explicit `(?x:…)` wrapper:
/// `g_regex_flags = VERBOSE` compiles every terminal under a global `(?x)`
/// prefix while the lookaround analyzer still counts whitespace/comments as
/// literal width — the exact same class. The routing seam must refuse any
/// lookaround pattern under global VERBOSE with the same categorized NYI
/// `VerboseMode` error, on BOTH combined-scanner builds (and, via the seam,
/// every other engine path). A verbose *plain* pattern never reaches the seam
/// (the `regex` crate compiles `(?x)` natively) and must keep building.
#[test]
fn global_verbose_flag_refuses_lookaround_lowering() {
    use crate::grammar::terminal::flags;
    use crate::lookaround::classify::{DeclineReason, LookaroundIssue, Scope};
    // No wrapper: the pattern looks analyzable, but under (?x) the space before
    // the guard is ignored at runtime while the analyzer would count it.
    let terms = [re_term(1, "VG", r"[0-9]+ (?![0-9])", 0)];
    let refs: Vec<(SymbolId, &TerminalDef)> = terms.iter().map(|(i, t)| (*i, t)).collect();
    let assert_refused = |result: Result<(), GrammarError>, engine: &str| match result {
        Err(GrammarError::LookaroundScope { scope, issue, .. }) => {
            assert_eq!(scope, Scope::NotYetImplemented, "{engine}");
            assert_eq!(
                issue,
                LookaroundIssue::Declined(DeclineReason::VerboseMode),
                "{engine}"
            );
        }
        Err(other) => panic!("{engine}: expected the VerboseMode scope error, got {other:?}"),
        Ok(()) => {
            panic!("{engine}: a global-VERBOSE lookaround terminal built — the false-accept class")
        }
    };
    assert_refused(
        DfaScanner::build(&refs, flags::VERBOSE).map(drop),
        "DfaScanner",
    );
    // The Regex backend refuses identically — including under the TEST-ONLY
    // `fancy-oracle` feature, whose build routes the same seam first (PR #137
    // review, blocker 2: the feature must never widen the accepted grammar set).
    assert_refused(Scanner::build(&refs, flags::VERBOSE).map(drop), "Scanner");
    // Without an assertion there is no hazard: a verbose plain pattern compiles
    // on the `regex` crate and never reaches the routing seam.
    let plain = [re_term(1, "VP", r"[0-9]+ [a-z]+", 0)];
    let prefs: Vec<(SymbolId, &TerminalDef)> = plain.iter().map(|(i, t)| (*i, t)).collect();
    DfaScanner::build(&prefs, flags::VERBOSE)
        .expect("a plain pattern under global VERBOSE builds (the regex crate handles `x`)");
    Scanner::build(&prefs, flags::VERBOSE)
        .expect("a plain pattern under global VERBOSE builds (the regex crate handles `x`)");
}

#[test]
fn dfa_all_lookaround_terminals_is_a_categorized_out_of_scope_error() {
    // A scanner whose only terminal is an *internal*-assertion lookaround pattern
    // (not a lowerable boundary shape, not a recognized idiom) refuses to build
    // with the categorized OutOfScope error — since L4 there is no fancy
    // side-probe to ride.
    use crate::lookaround::classify::Rejection;
    use crate::lookaround::classify::{LookaroundIssue, Scope};
    let terms = [re_term(1, "STR", "\"(?!\")[^\"]*\"", 0)];
    assert_dfa_scope_error(
        &terms,
        Scope::OutOfScope,
        LookaroundIssue::Rejected(Rejection::Internal),
    );
}
