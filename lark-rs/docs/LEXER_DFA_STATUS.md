# Lexer DFA — per-terminal route status

*Companion to [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md). Status as of 2026-06-10 (after
**L4 landed**: refusals are categorized build errors — see
[`LOOKAROUND_SCOPE.md`](LOOKAROUND_SCOPE.md) — and `fancy-regex` left the runtime).*

This is the planning-only census of where each terminal **shape** routes on master. The
routes are defined in the plan's "Runtime routing taxonomy" (Plain / Lowered / Declined /
Rejected / Invalid). They are a **typed enum**,
`classify::LoweringRoute::{Plain, Lowered, Declined, Unsupported, Invalid}`, returned by
`route_terminal_dotall` and matched directly by `DfaScanner::build`; the per-route pins
live in `tests/test_lowering_routes.rs`. The executable pins of this table for the bundled
lookaround terminals are
`tests/test_string_splice.rs::bundled_lookaround_terminal_lowering_status` (route level)
and `lexer::tests::dfa_bundled_lookaround_terminals_lower_with_no_fancy_probe` (engine
path) — if either goes red, this table is out of date.

**Route enum vs. runtime outcome (since L4: identical).** Every refusal — `Declined` or
`Unsupported` — IS the runtime outcome: a categorized `GrammarError::LookaroundScope`
carrying the two-category scope (`OutOfScope` / `NotYetImplemented`,
[`LOOKAROUND_SCOPE.md`](LOOKAROUND_SCOPE.md)), produced by the single
`lexer::route_fancy_only_terminal` seam and scoreboarded end-to-end by
`tests/test_lookaround_scope.rs`. There is no fallback engine.

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
| Unbounded leading-boundary | `(?!\[=*\[)[^\s]+` (gersemi UNQUOTED_ELEMENT) | **Lowered** (M2 — a leading guard runs anchored at match-start, so its body width never affects the accept position; per-attempt guard cost becomes O(remaining), Python-`re` parity) | classify pins + reject-corpus split (leading supported / trailing rejected) + `test_wild_gap_pins.rs` runtime pin | — | no |
| Fence idiom (tag-echo backref) | `<<(?P<t>…)\n(?:.\|\n)*?(?P=t)` (hcl2 heredoc), `\[(?P<e>(=*))\[([\s\S]+?)\](?P=e)\]` (gersemi BRACKET_ARGUMENT) | **Matched outside the DFA** (idiom #5, `lexer/fence.rs` two-phase scanner; non-regular, so it *bypasses* the refusal seam; recognizer demands literal sections + a universal lazy body and honors the `+?` body minimum) | recognizer accept/reject pins + matcher Python-parity pins (`[[]]` is a lex error) + `test_wild_gap_pins.rs` end-to-end + the `\G` fancy probe stays the independent oracle under `fancy-oracle` | — | no |
| Fixed-offset bounded lookbehind | `(?<!_)/`, `\w(?<!_)x`, `(?<=ab)c` | **Lowered** (M3, backward guard at fixed offset) | M3 generative + lookbehind mutation + Route-1 | — | no |
| `python.STRING` opening-guard splice | `([ubf]?r?\|r[ubf])("(?!"")…"\|'(?!'')…')` | **Lowered** (M4, `recognize_string_idiom`; engages on the engine path since the flag-wrapper strip) | `""""`/`"" ""` canary + Route-1 nested + python.lark differential + zero-fancy-probe pin | — | no |
| `lark.REGEXP` regex-literal idiom | `\/(?!\/)(\\\/\|\\\\\|[^\/])*?\/[imslux]*` | **Lowered** (Stage B, `recognize_regexp_idiom` — the `(?!\/)` reduces to a non-empty-body `*?`→`+?` bump; one unguarded branch) | `//`/lazy-close/dangling-escape/flags canaries (`test_regexp_splice.rs`) + generative equivalence + `*?`-mutant + state-pruned Route-1 + differential population + lark.lark files | — | no |
| `python.LONG_STRING` long-string idiom | `…(""".*?(?<!\\)(\\\\)*?"""\|…)` | **Lowered** (Stage B, `recognize_long_string_idiom` — the escape-pair body normalization `(?:[^\\<nl>]\|\\.)*?` absorbs the `(?<!\\)(\\\\)*?` parity close; lazy `*?` kept; two unguarded branches; no delimiter automaton needed) | empty/quote-run/parity/newline canaries + exhaustive dotall backend differential (`test_long_string_splice.rs`) + generative equivalence + parity/two-quote/greedy mutants + state-pruned Route-1 + differential population + python.lark docstrings + stdlib oracles | — | no |
| Loader-wrapped trailing guard (terminal algebra) | `T: "a" /(?!x)/ \| "b"` → `(?:a(?!x))\|(?:b)` | **Lowered** (the vacuous whole-arm `(?:…)` wrapper is unwrapped — `unwrap_vacuous_groups`, `(?:X) ≡ X` — then M1) | recovery tests (`test_lookaround.rs`) + stdlib oracles | — | no |
| `python.DEC_NUMBER` guarded arm | `0(?:(?:_)?0)*(?![1-9])` | **Lowered** (M1 via the exact `is_leftmost_longest` semantic realizability gate — both syntactic fast paths miss it) | gate unit pins + exhaustive generative equivalence (`dec_number_loader_shape_lowered_equals_fancy`, 0 divergences) + stdlib oracles + differential | — | no |
| Per-instance decline (user grammar) | `\w+(?<!_)x` (variable-offset lookbehind), `(ab\|abc)(?!z)` (non-realizable guarded base), `(?x:…)` wrappers | **Declined** → categorized **build error**, `Scope::NotYetImplemented` — a clean refusal, never a mis-lowering; the scoreboard rows are promotion tripwires | scoreboard (`test_lookaround_scope.rs`) + route pin + lexer unit pins | promote per-shape via the Stage-B ladder ([`LOOKAROUND_SCOPE.md`](LOOKAROUND_SCOPE.md) protocol) | n/a (landed) |
| Unsupported internal lookahead (user grammar) | `a(?=b)c`, the block-comment `(\*(?!\/)\|[^*])*` | **Unsupported(Internal)** → categorized **build error**, `Scope::OutOfScope` (by design; the audited idioms are the growth path; named parity break — asserted on every parser×lexer combo) | scoreboard + the asserted-rejection oracle group (`test_lookaround.rs::OUT_OF_SCOPE_GROUPS`) + reject corpus | — (permanent) | n/a (landed) |
| Backref / nested / unbounded-trailing / variable-width lookbehind | `(?=\1)`, `(a)\1`, `(?=(?!a)b)`, `Y(?![ ]*X)`, `(?<!a*)b` | **Unsupported / Declined(BacktrackingOnlySyntax)** → categorized **build error** (`OutOfScope`, except unbounded trailing lookahead = `NotYetImplemented`; the exact fence idiom and *leading* unbounded lookahead are now the two carve-outs above) | scoreboard + reject corpus + mutation meta-test + route pin | — (permanent; unbounded trailing context promotable) | n/a (landed) |

## Reading the "Blocks L4?" column

**L4 has landed.** The decline-vs-reject contract is typed AND enforced: every refusal is
a categorized build error under the two-category scope taxonomy
([`LOOKAROUND_SCOPE.md`](LOOKAROUND_SCOPE.md)), `fancy-regex` is out of `[dependencies]`
(dev-dependency oracle + the default-OFF TEST-ONLY `fancy-oracle` feature for the L0
differential's reference backend), and every bundled lookaround terminal — including the
loader-baked `python.DEC_NUMBER` the flip surfaced — lowers on the engine path. The
column is retained for history; nothing blocks L4 any more. Next: **L5** (bake the
serialized scanner bundle). See [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md) L4/L5.
