# Lexer DFA — terminal/shape status matrix

*Planning-only companion to [`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md). This table is the
one-glance "what routes where on `master`" reference. Update it in the same PR as any
routing change (see the plan's "Next implementation PR checklist").*

Routes use the plan's [routing taxonomy](LEXER_DFA_PLAN.md#runtime-routing-taxonomy):
**Plain**, **Lowered**, **Declined-to-fancy**, **Rejected**.

| Terminal / shape | Example pattern | Current route on master | Coverage | Next step | Blocks L4? |
|---|---|---|---|---|---|
| Plain terminals (no lookaround) | `[a-z]+`, `[0-9]+` | **Plain** (DFA) | scanner differential + full bank | — | No |
| `lark.OP` (trailing guard, per-branch) | `[+*]\|[?](?![a-z])` | **Lowered** (M1) | generative equiv + Route-1 proof + reject corpus | — | No |
| `common.DEC_NUMBER` (trailing guard, length-changing) | `0(?![1-9])` / `[1-9][0-9]*` | **Lowered** (M1) | generative equiv (incl. length-change) + proof | — | No |
| Fixed leading-boundary | `(?!if\|else)[a-z]+`, `(?=[A-Z])[a-z]+` | **Lowered** (M2) | generative equiv + Route-1 proof | — | No |
| Fixed-offset bounded lookbehind | `(?<!_)/`, `(?<====)x` | **Lowered** (M3) | generative equiv + lookbehind mutation meta-tests + proof | — | No |
| `python.STRING` (opening-guard splice) | `([ubf]?r?\|r[ubf])("(?!"").*?(?<!\\)(\\\\)*?"\|…)` | **Lowered** (M4 idiom) | `""""`/`"" ""` canary + real-nested Route-1 proof + python.lark differential | — | No |
| `python.LONG_STRING` | `(…)(""".*?(?<!\\)(\\\\)*?"""\|…)` | **Declined-to-fancy** | declined-route tripwire (`test_string_splice.rs`) | delimited-token **long-string** idiom (plan Stage B) | **Yes** |
| `lark.REGEXP` | `\/(?!\/)(\\\/\|\\\\\|[^\/])*?\/[imslux]*` | **Declined-to-fancy** | declined-route tripwire (`test_string_splice.rs`) | delimited-token **regex-literal** idiom (plan Stage B) | **Yes** |
| Unsupported *internal* lookahead in a user grammar | `a(?=b)c` | classifier **Rejects**, but build path currently absorbs it into the `fancy-regex` fallback (design debt) | classifier reject tests | resolve decline-vs-reject contract so this is a **loud build error** in the final L4 world | **Yes** (contract must be resolved) |
| Backrefs / nested / unbounded / variable-width-behind assertions | `(a)(?=\1)`, `(?=(?!a)b)`, `(?![ ]*X)`, `(?<!a*)b` | **Rejected** | reject corpus + mutation meta-test | — (permanent reject) | — |

## How to read "Blocks L4?"

L4 (remove `fancy-regex` from the runtime) cannot happen while anything the **bundled**
grammars need still routes to `fancy-regex` — that is `python.LONG_STRING` and
`lark.REGEXP` today. It also cannot happen while the **decline-vs-reject contract** for
user grammars is unresolved (a permanent rejection is currently absorbed into the
fallback instead of failing the build). Both must be cleared. See
[`LEXER_DFA_PLAN.md`](LEXER_DFA_PLAN.md) "L4" and the design-debt note.
