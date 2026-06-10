# Lexer DFA — per-terminal route status

*Companion to [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md). Status as of 2026-06-10 (after
the `python.LONG_STRING` Stage-B lowering + the flag-wrapper strip).*

This is the planning-only census of where each terminal **shape** routes on master. The
routes are defined in the plan's "Runtime routing taxonomy" (Plain / Lowered /
Declined-to-fancy / Rejected / Invalid). They are now a **typed enum**,
`classify::LoweringRoute::{Plain, Lowered, DeclinedToFancy, Unsupported, Invalid}`, returned
by `route_terminal_dotall` and matched directly by `DfaScanner::build`; the per-route pins
live in `tests/test_lowering_routes.rs`. The executable pins of this table for the bundled
lookaround terminals are
`tests/test_string_splice.rs::bundled_lookaround_terminal_lowering_status` (route level)
and `lexer::tests::dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe` (engine
path: the built scanner has zero fancy side-probes) — if either goes red, this table is
out of date.

**Route enum vs. runtime outcome.** The "Route on master" column below describes the
*runtime* outcome (what engine the terminal lexes on). That is **not** always the same as
the `LoweringRoute` value: an `Unsupported` route still runs on `fancy-regex` today via the
compatibility fallback in `DfaScanner` (a single `push_fancy_fallback` seam) — so a user
grammar's internal lookahead lexes today even though its route is the L4 reject path. L4 is
the policy flip that makes the `Unsupported` route a build error.

**The flag-wrapper strip (2026-06-10).** The loader bakes a terminal's `/…/is`-style flags
into the pattern as one whole-pattern `(?is:…)` wrapper with `PatternRe.flags = 0`, so the
router used to see the bundled idioms' assertions nested inside a `Group` — the wrapped
`python.STRING` silently rode the `Unsupported` compatibility fallback at runtime (its M4
route-level proofs held, on the unwrapped constants; the differential could not surface it
because the fancy reference agreed by construction). `DfaScanner::build` now strips the
wrapper back into the flag bitset (`strip_whole_pattern_flag_wrapper`) before routing and
re-applies it to every lowered branch/guard, and threads `g_regex_flags` DOTALL into the
lowering the same way — so the rows below describe the **engine path**, not just the
route-level constants.

| Terminal / shape | Example pattern | Route on master | Coverage | Next step | Blocks L4? |
|---|---|---|---|---|---|
| Plain terminal (no lookaround) | `[a-z]+[0-9]*` | **Plain** (leftmost-first DFA) | scanner differential | — | no |
| `lark.OP` trailing guard | `[+*]\|[?](?![a-z])` | **Lowered** (M1, per-branch guarded accept) | M1 generative + Route-1 + reject + differential | — | no |
| `common.DEC_NUMBER` trailing guard | `…0(_?0)*(?![1-9])` | **Lowered** (M1, length-changing guard) | M1 generative + Route-1 + differential | — | no |
| Fixed leading-boundary | `(?!--)[a-z]+`, `(?=[A-Z])[a-z]+` | **Lowered** (M2, start precondition) | M2 generative + Route-1 + reject | — | no |
| Fixed-offset bounded lookbehind | `(?<!_)/`, `\w(?<!_)x`, `(?<=ab)c` | **Lowered** (M3, backward guard at fixed offset) | M3 generative + lookbehind mutation + Route-1 | — | no |
| `python.STRING` opening-guard splice | `([ubf]?r?\|r[ubf])("(?!"")…"\|'(?!'')…')` | **Lowered** (M4, `recognize_string_idiom`; engages on the engine path since the flag-wrapper strip) | `""""`/`"" ""` canary + Route-1 nested + python.lark differential + zero-fancy-probe pin | — | no |
| `lark.REGEXP` regex-literal idiom | `\/(?!\/)(\\\/\|\\\\\|[^\/])*?\/[imslux]*` | **Lowered** (Stage B, `recognize_regexp_idiom` — the `(?!\/)` reduces to a non-empty-body `*?`→`+?` bump; one unguarded branch) | `//`/lazy-close/dangling-escape/flags canaries (`test_regexp_splice.rs`) + generative equivalence + `*?`-mutant + state-pruned Route-1 + differential population + lark.lark files | — | no |
| `python.LONG_STRING` long-string idiom | `…(""".*?(?<!\\)(\\\\)*?"""\|…)` | **Lowered** (Stage B, `recognize_long_string_idiom` — the escape-pair body normalization `(?:[^\\<nl>]\|\\.)*?` absorbs the `(?<!\\)(\\\\)*?` parity close; lazy `*?` kept; two unguarded branches; no delimiter automaton needed) | empty/quote-run/parity/newline canaries + exhaustive dotall backend differential (`test_long_string_splice.rs`) + generative equivalence + parity/two-quote/greedy mutants + state-pruned Route-1 + differential population + python.lark docstrings + stdlib oracles | — | no |
| Per-instance decline (user grammar) | `\w+(?<!_)x` (variable-offset lookbehind), `(ab\|abc)(?!z)` (non-realizable guarded base) | **Declined-to-fancy** (`LoweringRoute::DeclinedToFancy`) — runs on `fancy-regex`, correct, never mis-lowered | route pin (`test_lowering_routes.rs`) + lexer unit declines | decide at L4: error vs. documented fancy support | **yes** |
| Unsupported internal lookahead (user grammar) | `a(?=b)c`, `(?:X(?=Y))*` | `LoweringRoute::Unsupported(Internal)`; the build path's compatibility fallback still routes it to `fancy-regex` (so it lexes today, masking the reject) | reject corpus (`test_lowering_reject.rs`) + route pin (`test_lowering_routes.rs`) | **flip the policy:** make the `Unsupported` arm a build error (plan, "Runtime routing taxonomy") | **yes** (contract) |
| Backref / nested / unbounded / variable-width lookbehind | `(?=\1)`, `(?=(?!a)b)`, `(?![ ]*X)`, `(?<!a*)b` | `LoweringRoute::Unsupported(Backref/Nested/Unbounded/VariableWidthBehind)`; the scanner compatibility fallback may still route it to `fancy-regex` (it compiles there for backref/nested; an unbounded/variable-width body may then fail to compile → build error) — *not* a permanent reject yet | reject corpus + mutation meta-test + route pin (`test_lowering_routes.rs`) | flip the `Unsupported` arm to a build error before L4 | **yes** (contract) |

## Reading the "Blocks L4?" column

L4 (drop runtime `fancy-regex`) is blocked until the **decline-vs-reject contract** is
*enforced* (an unsupported user lookaround should error, not silently route to fancy) and
a policy is chosen for per-instance user declines. The bundled-terminal gate has
**cleared**: every bundled lookaround terminal (`STRING`, `REGEXP`, `LONG_STRING`) lowers
on the engine path, so no bundled terminal rides `fancy-regex` any more. The result type
is split — `LoweringRoute` separates `Unsupported` from `DeclinedToFancy`, so the contract
is **typed** — but the runtime policy is **not yet flipped**: `Unsupported` still rides the
compatibility fallback. The one remaining gate is that policy flip. See
[`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md) L4/L5 and the "Next implementation PR checklist".
