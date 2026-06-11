//! **THE single refusal seam** (L4): the one place a `regex`-crate-rejected
//! terminal is routed through the typed lowering or refused with the
//! categorized scope error (`docs/LOOKAROUND_SCOPE.md`).

use super::pattern::{
    is_verbose_wrapped_lookaround, pattern_contains_assertion, strip_whole_pattern_flag_wrapper,
};
use crate::error::GrammarError;
use crate::grammar::terminal::{Pattern, TerminalDef};

/// Route one `regex`-crate-rejected terminal through the typed lowering and either
/// return its lowered branches (+ the merged flag bitset to re-wrap them with), or the
/// **categorized scope build error** (`GrammarError::LookaroundScope`,
/// `docs/LOOKAROUND_SCOPE.md`). The successor of the historical `push_fancy_fallback`
/// compatibility seam: every refusal — a per-instance decline, an out-of-shape
/// rejection, or backtracking-only syntax — funnels through exactly this function, on
/// every engine (`DfaScanner`, the `Scanner` reference backend's default build, the
/// Earley `DynamicMatcher`, and `compute_unless`), so the categorized error is
/// produced in one auditable place.
///
/// `compile_err` is the `regex` crate's rejection message for the full source — quoted
/// in the backtracking-only triage so the user sees the engine's own reason.
pub(super) fn route_fancy_only_terminal(
    def: &TerminalDef,
    global_flags: u32,
    compile_err: &str,
) -> Result<(Vec<crate::lookaround::lower::LoweredBranch>, u32), GrammarError> {
    use crate::lookaround::classify::{
        route_terminal_dotall, scope_message, DeclineReason, LookaroundIssue, LoweringRoute,
    };
    let (raw, flags) = match &def.pattern {
        Pattern::Re(p) => (p.pattern.as_str(), p.flags),
        // A string literal compiles as an escaped plain pattern and never reaches this
        // seam; error defensively rather than panicking.
        Pattern::Str(_) => {
            return Err(GrammarError::InvalidRegex {
                pattern: def.name.clone(),
                reason: format!("string terminal failed to compile: {compile_err}"),
            });
        }
    };
    // The loader bakes terminal-level `/…/is` flags into the pattern as one
    // whole-pattern wrapper (`(?is:…)`, `PatternRe.flags = 0`); strip it back into the
    // flag bitset so the lowering sees the assertions at top level. The caller re-wraps
    // every lowered branch/guard with the returned `flags`.
    let (raw, flags) = strip_whole_pattern_flag_wrapper(raw, flags);
    let raw = raw.as_str();
    // VERBOSE mode makes the lookaround analyzer's arithmetic wrong (whitespace and
    // comments are counted as literal width while the compiled branch ignores them —
    // the false-accept class), in either of its two spellings:
    //   * a whole-pattern `(?x:…)` wrapper — the strip refused it (VERBOSE bodies
    //     are not analyzable), caught before the classifier can mislabel the
    //     group-nested assertion as internal lookahead;
    //   * the global `g_regex_flags` VERBOSE bit (or a terminal-level `x` flag bit) —
    //     the raw pattern looks plain to the analyzer but compiles under `(?x)`,
    //     so any pattern containing an assertion must be refused before routing.
    // Both refuse with the honest categorized NYI reason.
    let verbose_mode = ((flags | global_flags) & crate::grammar::terminal::flags::VERBOSE) != 0;
    if is_verbose_wrapped_lookaround(raw) || (verbose_mode && pattern_contains_assertion(raw)) {
        let reason = DeclineReason::VerboseMode;
        return Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: raw.to_string(),
            scope: reason.scope(),
            issue: LookaroundIssue::Declined(reason),
            msg: scope_message(
                &def.name,
                raw,
                LookaroundIssue::Declined(reason),
                reason.explain(),
            ),
        });
    }
    // `dotall` must reflect the terminal's own flags *or* the global `(?s…)` prefix —
    // both end up wrapped around every lowered branch source.
    let dotall = ((flags | global_flags) & crate::grammar::terminal::flags::DOTALL) != 0;
    match route_terminal_dotall(&def.name, raw, dotall) {
        LoweringRoute::Lowered(branches) => Ok((branches, flags)),
        LoweringRoute::Declined { reason, message } => Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: raw.to_string(),
            scope: reason.scope(),
            issue: LookaroundIssue::Declined(reason),
            msg: message,
        }),
        LoweringRoute::Unsupported {
            assertion,
            rejection,
            message,
        } => Err(GrammarError::LookaroundScope {
            terminal: def.name.clone(),
            subject: assertion,
            scope: rejection.scope(),
            issue: LookaroundIssue::Rejected(rejection),
            msg: message,
        }),
        // No lookaround at all, yet the `regex` crate rejected the pattern:
        // backtracking-only syntax (a top-level backreference, an atomic group, a
        // possessive quantifier) — a by-design non-goal (and, for backrefs, the one
        // named parity break with Python Lark's backtracking engine).
        LoweringRoute::Plain => {
            let reason = DeclineReason::BacktrackingOnlySyntax;
            Err(GrammarError::LookaroundScope {
                terminal: def.name.clone(),
                subject: raw.to_string(),
                scope: reason.scope(),
                issue: LookaroundIssue::Declined(reason),
                msg: scope_message(
                    &def.name,
                    raw,
                    LookaroundIssue::Declined(reason),
                    &format!(
                        "{} (the regex engine said: {compile_err})",
                        reason.explain()
                    ),
                ),
            })
        }
        LoweringRoute::Invalid { message } => Err(GrammarError::Other { msg: message }),
    }
}
