# Lexer DFA — per-terminal route status

*Companion to [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md). Status as of 2026-06-10 (after
the `lark.REGEXP` Stage-B lowering).*

This is the planning-only census of where each terminal **shape** routes on master. The
routes are defined in the plan's "Runtime routing taxonomy" (Plain / Lowered /
Declined-to-fancy / Rejected / Invalid). They are now a **typed enum**,
`classify::LoweringRoute::{Plain, Lowered, DeclinedToFancy, Unsupported, Invalid}`, returned
by `route_terminal_dotall` and matched directly by `DfaScanner::build`; the per-route pins
live in `tests/test_lowering_routes.rs`. The executable pin of this table for the bundled
lookaround terminals is
`tests/test_string_splice.rs::bundled_lookaround_terminal_lowering_status` — if that test
goes red, this table is out of date.

**Route enum vs. runtime outcome.** The "Route on master" column below describes the
*runtime* outcome (what engine the terminal lexes on). That is **not** always the same as
the `LoweringRoute` value: an `Unsupported` route still runs on `fancy-regex` today via the
compatibility fallback in `DfaScanner` (a single `push_fancy_fallback` seam) — an
out-of-shape *user* lookahead lexes on `fancy-regex` while its route says `Unsupported`.
L4 is the policy flip that makes the `Unsupported` route a build error.

| Terminal / shape | Example pattern | Route on master | Coverage | Next step | Blocks L4? |
|---|---|---|---|---|---|
| Plain terminal (no lookaround) | `[a-z]+[0-9]*` | **Plain** (leftmost-first DFA) | scanner differential | — | no |
| `lark.OP` trailing guard | `[+*]\|[?](?![a-z])` | **Lowered** (M1, per-branch guarded accept) | M1 generative + Route-1 + reject + differential | — | no |
| `common.DEC_NUMBER` trailing guard | `…0(_?0)*(?![1-9])` | **Lowered** (M1, length-changing guard) | M1 generative + Route-1 + differential | — | no |
| Fixed leading-boundary | `(?!--)[a-z]+`, `(?=[A-Z])[a-z]+` | **Lowered** (M2, start precondition) | M2 generative + Route-1 + reject | — | no |
| Fixed-offset bounded lookbehind | `(?<!_)/`, `\w(?<!_)x`, `(?<=ab)c` | **Lowered** (M3, backward guard at fixed offset) | M3 generative + lookbehind mutation + Route-1 | — | no |
| `python.STRING` opening-guard splice | `([ubf]?r?\|r[ubf])("(?!"")…"\|'(?!'')…')` | **Lowered** (M4, `recognize_string_idiom`) | `""""`/`"" ""` canary + Route-1 nested + python.lark differential | — | no |
| `python.LONG_STRING` | `…(""".*?(?<!\\)(\\\\)*?"""\|…)` | **Declined-to-fancy** | runs on `fancy-regex`; equivalence pinned by `test_lookaround.rs` | audited **delimited-token** long-string idiom (Stage B) with a multi-char `"""` delimiter automaton | **yes** |
| `lark.REGEXP` regex-literal idiom | `\/(?!\/)(\\\/\|\\\\\|[^\/])*?\/[imslux]*` | **Lowered** (Stage B, `recognize_regexp_idiom` — one unguarded branch, the proven Type-A rewrite `\/(\\\/\|\\\\\|[^\/])+\/[imslux]*`) | `test_regexp_splice.rs` canaries (`//` reject, `/a//` non-swallow) + state-pruned Route-1 + generative equivalence + `matchlen::regexp_match_length_equivalence` + lark.lark differential | — | no |
| Unsupported internal lookahead (user grammar) | `a(?=b)c`, `(?:X(?=Y))*` | `LoweringRoute::Unsupported(Internal)`; the build path's compatibility fallback still routes it to `fancy-regex` (so it lexes today, masking the reject) | reject corpus (`test_lowering_reject.rs`) + route pin (`test_lowering_routes.rs`) | **flip the policy:** make the `Unsupported` arm a build error (plan, "Runtime routing taxonomy") | **yes** (contract) |
| Backref / nested / unbounded / variable-width lookbehind | `(?=\1)`, `(?=(?!a)b)`, `(?![ ]*X)`, `(?<!a*)b` | `LoweringRoute::Unsupported(Backref/Nested/Unbounded/VariableWidthBehind)`; the scanner compatibility fallback may still route it to `fancy-regex` (it compiles there for backref/nested; an unbounded/variable-width body may then fail to compile → build error) — *not* a permanent reject yet | reject corpus + mutation meta-test + route pin (`test_lowering_routes.rs`) | flip the `Unsupported` arm to a build error before L4 | **yes** (contract) |

## Reading the "Blocks L4?" column

L4 (drop runtime `fancy-regex`) is blocked while **any** row is *Declined-to-fancy*, and
separately until the **decline-vs-reject contract** is *enforced* (an unsupported user
lookaround should error, not silently route to fancy). The result type is now split —
`LoweringRoute` separates `Unsupported` from `DeclinedToFancy`, so the contract is **typed**
— but the runtime policy is **not yet flipped**: `Unsupported` still rides the compatibility
fallback. So the two remaining gates are: lower `python.LONG_STRING` (the last bundled
decline — `lark.REGEXP` now lowers), *and* flip the `Unsupported` route to a build error.
See [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md) L4/L5 and the "Next implementation PR
checklist".
