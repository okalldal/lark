# Lexer DFA — per-terminal route status

*Companion to [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md). Status as of 2026-06-09 (after
PR #124).*

This is the planning-only census of where each terminal **shape** routes on master. The
routes are defined in the plan's "Runtime routing taxonomy" (Plain / Lowered /
Declined-to-fancy / Rejected / Invalid). The executable pin of this table for the bundled
lookaround terminals is
`tests/test_string_splice.rs::bundled_lookaround_terminal_lowering_status` — if that test
goes red, this table is out of date.

| Terminal / shape | Example pattern | Route on master | Coverage | Next step | Blocks L4? |
|---|---|---|---|---|---|
| Plain terminal (no lookaround) | `[a-z]+[0-9]*` | **Plain** (leftmost-first DFA) | scanner differential | — | no |
| `lark.OP` trailing guard | `[+*]\|[?](?![a-z])` | **Lowered** (M1, per-branch guarded accept) | M1 generative + Route-1 + reject + differential | — | no |
| `common.DEC_NUMBER` trailing guard | `…0(_?0)*(?![1-9])` | **Lowered** (M1, length-changing guard) | M1 generative + Route-1 + differential | — | no |
| Fixed leading-boundary | `(?!--)[a-z]+`, `(?=[A-Z])[a-z]+` | **Lowered** (M2, start precondition) | M2 generative + Route-1 + reject | — | no |
| Fixed-offset bounded lookbehind | `(?<!_)/`, `\w(?<!_)x`, `(?<=ab)c` | **Lowered** (M3, backward guard at fixed offset) | M3 generative + lookbehind mutation + Route-1 | — | no |
| `python.STRING` opening-guard splice | `([ubf]?r?\|r[ubf])("(?!"")…"\|'(?!'')…')` | **Lowered** (M4, `recognize_string_idiom`) | `""""`/`"" ""` canary + Route-1 nested + python.lark differential | — | no |
| `python.LONG_STRING` | `…(""".*?(?<!\\)(\\\\)*?"""\|…)` | **Declined-to-fancy** | runs on `fancy-regex`; equivalence pinned by `test_lookaround.rs` | audited **delimited-token** long-string idiom (Stage B) with a multi-char `"""` delimiter automaton | **yes** |
| `lark.REGEXP` | `\/(?!\/)(\\\/\|\\\\\|[^\/])*?\/[imslux]*` | **Declined-to-fancy** | runs on `fancy-regex` | audited **delimited-token** regex-literal idiom (Stage B): single-char `/` delimiter, internal `(?!\/)`, escaped-slash body, trailing flags | **yes** |
| Unsupported internal lookahead (user grammar) | `a(?=b)c`, `(?:X(?=Y))*` | **Classifier rejects**; the build path's compatibility fallback currently routes the `Err` to `fancy-regex` (so it lexes today, masking the reject) | reject corpus (`test_lowering_reject.rs`) pins the *classifier* verdict | resolve the decline-vs-reject contract so the runtime errors loudly (plan, "Runtime routing taxonomy") | **yes** (contract) |
| Backref / nested / unbounded / variable-width lookbehind | `(?=\1)`, `(?=(?!a)b)`, `(?![ ]*X)`, `(?<!a*)b` | **Classifier rejects**; the current scanner compatibility fallback may still route the `Err` to `fancy-regex` (it compiles there for backref/nested; an unbounded/variable-width body may then fail to compile → `InvalidRegex`) — *not* a permanent build error yet | reject corpus + mutation meta-test pin the classifier verdict | split `Lowered` / `DeclineToFancy` / `Unsupported` before L4 so these error loudly | **yes** (contract) |

## Reading the "Blocks L4?" column

L4 (drop runtime `fancy-regex`) is blocked while **any** row is *Declined-to-fancy*, and
separately while the **decline-vs-reject contract** is unresolved (an unsupported user
lookaround should error, not silently route to fancy). Lowering `python.LONG_STRING` and
`lark.REGEXP`, *and* splitting the lowerer's result type so unsupported ≠ declined, are the
two gates. See [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md) L4/L5 and the
"Next implementation PR checklist".
