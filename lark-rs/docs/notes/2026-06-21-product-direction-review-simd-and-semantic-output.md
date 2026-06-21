# SIMD parser optimization query

## Metadata

- Source archive: `SIMD parser optimization query.webarchive`
- Source URL: https://chatgpt.com/share/6a3778be-2280-83eb-8b16-2709473dcd2d
- Conversation range: 2026-06-20T23:10:17+02:00 to 2026-06-21T07:35:47+02:00
- Share/archive created: 2026-06-21T07:38:06+02:00
- Share/archive updated: 2026-06-21T07:38:32+02:00
- Exported: 2026-06-21T08:09:04+02:00
- Messages included: 14 user/assistant messages

## Extraction notes

- Included public-facing `user` and `assistant` text messages from the embedded conversation data.
- Omitted system messages, tool calls/tool outputs, hidden reasoning/thought entries, and the placeholder “Original custom instructions no longer available”.
- ChatGPT-specific citation glyphs were normalized to `[filecite: …]` / `[cite: …]` style markers so ordinary Markdown editors show them legibly.

Omitted item counts:

- system: 2
- tool: 103
- assistant_thoughts: 17
- assistant_tool_calls: 82
- placeholder_user_context: 1
- other: 4

## Conversation

---

**1. User** · `2026-06-20T23:10:17+02:00`

GitHub what would it take to crass the simd bridge for okalldal/lark? Would it be possible to build something that takes a generic grammar and compiles a simd optimized parser for it, on par with dedicated libraries for eg json and stuff?

---

**2. Assistant** · `2026-06-20T23:10:31+02:00` · `gpt-5-5-pro`

I’ll treat “crass the SIMD bridge” as “cross/build the SIMD bridge,” inspect `okalldal/lark`, then separate concrete integration work from the broader generic grammar-to-SIMD feasibility question.

---

**3. Assistant** · `2026-06-20T23:13:34+02:00` · `gpt-5-5-pro`

The fork looks like ordinary pure-Python Lark: Earley, Look-Ahead Left-to-Right (LALR), Cocke–Younger–Kasami (CYK), several lexers, and standalone LALR generation. A SIMD prototype likely fits through replaceable lexer/parser hooks, especially `BasicLexer`, `ContextualLexer`, `LexerThread`, and `LALR_Parser`, without changing `Lark(...)`.

---

**4. Assistant** · `2026-06-20T23:16:09+02:00` · `gpt-5-5-pro`

Yes — but **not** as “any generic grammar goes in, simdjson-class parser comes out.” The sane target is a **tiered grammar compiler**:

1. Always preserve Lark semantics.
2. Detect when a grammar is eligible for stronger generated fast paths.
3. Emit SIMD-assisted scanners and fused parser/lexer code for those grammars.
4. Fall back to the existing general path when the grammar is too dynamic, ambiguous, or tree-heavy.

For `okalldal/lark`, the interesting news is that the repo already has most of the **DFA bridge** crossed in `lark-rs`. The current lexer path has a `ScannerBackend` seam, a default `regex-automata` `DfaScanner`, contextual lexer narrowing, interned token IDs, and a combined scanner design rather than one regex call per terminal. The docs say the DFA backend, bounded-lookaround lowering, default-DFA flip, and removal of runtime `fancy-regex` have landed; the remaining “bakeability” task is serializing the whole scanner bundle into standalone targets. [filecite: turn24file0] [filecite: turn21file0] [filecite: turn22file0]

The frank version: **the next bridge is not “SIMD the parser loop.”** The Look-Ahead Left-to-Right (LALR) parser is already dense table lookup over interned IDs; the hot loop is state-stack dependent and inherently sequential. The real wins are in **byte classification, skipping, token materialization, allocation, and parser/lexer fusion**. The current parser table is already dense array-indexed, and the LALR loop is basically `peek token → action lookup → shift/reduce`; that is not where simdjson gets its magic. [filecite: turn33file0] [filecite: turn34file0]

## Why simdjson is a special beast

simdjson works so well because JSON has a tiny, fixed syntax surface: a small set of structural characters, strings, numbers, literals, whitespace, and predictable nesting. Its first stage uses SIMD to identify structural characters and quote/string masks, then later stages parse using those indexes. The paper explicitly describes the two-stage structure: find structural characters and pseudo-structural characters first, then process those indexes into a tape-like representation. [cite: turn668427view0 — Parsing Gigabytes of JSON per Second](https://arxiv.org/abs/1902.08318)

A generic Lark grammar is a different animal. Terminals can be arbitrary regular expressions, contextual lexing changes the valid token set by parser state, post-lexers can inject tokens, Earley and CYK support ambiguity/general context-free grammars, and Lark builds rich `Tree`/`Token` objects by default. That richness is the product — but it is exactly what stops a universal simdjson-style claim.

So the realistic promise is:

> **For JSON-like grammars and JSON-like output, yes, you can get close to dedicated parsers. For arbitrary Lark grammars, no, not on par with dedicated JSON libraries; but you can still get serious speedups.**

## What it would take in this repo

### 1. Finish the static DFA bundle first

Before SIMD, finish the current L5 bake step. The docs are very clear that standalone still bakes a regex `ScannerPlan`, while the actual scanner implementation is now a bundle: plain leftmost-first DFA, guarded all-matches DFA, guard DFAs/tables, lookbehind side tables, rank maps, start-byte prefilter, `unless` retyping, and `%ignore`. [filecite: turn22file0]

That matters because SIMD code wants **static, generated data**: byte-class masks, per-state token-start masks, ignore-run classifiers, delimiter sets, and fallback hooks. Trying to bolt SIMD onto the dynamic runtime first would be backwards.

### 2. Add a generated byte-class layer above `DfaScanner`

The current `DfaScanner` already has a start-byte prefilter: a `bool[256]` table saying whether the byte at `pos` can start any candidate token. It then runs the plain and guarded DFA engines, applies fences, and picks the best candidate by rank/branch order. [filecite: turn26file0] [filecite: turn27file0]

The SIMD bridge would generalize that prefilter from:

```text
check one byte at pos
```

to something like:

```text
scan 32 or 64 bytes
produce masks for:
  possible token starts
  whitespace / ignored chars
  newline positions
  string delimiters
  escape bytes
  structural chars
  comment starts
```

Then the lexer can skip long boring regions cheaply: whitespace, comments, runs of non-start bytes, and simple delimiter-driven tokens. For JSON, that becomes the structural-index pass. For ordinary languages, it is still valuable, but less dramatic.

This should be generated from the grammar, not hand-coded. The compiler can derive:

```text
FIRST_BYTE(terminals)
FIRST_BYTE(valid terminals in parser state)
ignored byte classes
newline-sensitive terminals
literal delimiter bytes
single-character terminals
quote/escape pairs
```

For contextual LALR, you get an even nicer target: **per parser state byte classes**. The contextual lexer already narrows candidate terminals by LALR state; the SIMD classifier should use those narrowed sets instead of a global “anything could start here” set. [filecite: turn24file0]

### 3. Fuse lexing and parsing for generated LALR parsers

The current design still has a token source abstraction: the parser asks for a token, then shifts/reduces. That is clean and general, but it leaves performance on the table. Research on generated parsers with fused lexing reports that separating lexer and parser can cost a lot through token materialization and dispatch, and that fused generated code can beat conventional parser generators by several times. [cite: turn149936academia0 — flap: A Deterministic Parser with Fused Lexing](https://arxiv.org/abs/2304.05276)

For `lark-rs`, the generated fast path should specialize by parser state:

```text
state 17:
  valid terminals = {STRING, RBRACE, COMMA}
  SIMD classify next bytes for these starts only
  run minimal matcher
  directly shift token span or reduce
```

That avoids:
- building a full `Token` object for punctuation that will be filtered anyway,
- storing owned token strings,
- running a general scanner when the parser state only admits two or three terminals,
- indirect `TokenSource` calls in the hottest path.

This is probably a bigger win than trying to vectorize the LR stack machine.

### 4. Stop allocating like a general-purpose tree builder when the user wants data

The repo’s own benchmark notes already say the LALR path is still short of the “10–100×” headline and that parse throughput is allocation-bound, with tree-representation work deferred. [filecite: turn30file0]

Right now `Token` owns `String` for both type name and value, and `Tree` owns a `String` label plus `Vec<Child>`. The shared `TreeBuilder` is already called out as the choke point where an arena or interned-label representation could be swapped in later. [filecite: turn35file0] [filecite: turn36file0]

For SIMD to matter, add output modes:

```text
Full Lark tree
  exact compatibility, slowest

Span tree
  token values are input ranges, labels are interned IDs

Arena tree
  bump-allocated nodes, compact children

Event/visitor mode
  no tree unless requested

Tape mode
  JSON-like structural output, closest to simdjson
```

A generic Lark tree builder will not compete with a dedicated JSON parser that writes a compact tape or typed document model. But a JSON grammar compiled to a JSON-like tape could.

### 5. Add a grammar profiler that assigns an acceleration tier

This is the key to making the feature honest.

A generated report should say something like:

```text
Grammar acceleration profile

Parser:
  LALR(1): yes
  contextual lexer: yes
  postlex: no
  ambiguity: no

Lexer:
  all terminals regular/lowered: yes
  start-byte sets finite/useful: yes
  ignored whitespace SIMD-skippable: yes
  string/fence fast path: yes
  Unicode-sensitive classes: no

Output:
  full tree required: no
  span/tape mode available: yes

Selected backend:
  simd_structural_lalr
```

And for a programming language grammar:

```text
Selected backend:
  simd_prefilter_lalr

Reason:
  postlex/indentation enabled
  rich tree required
  mixed regex terminals
  no structural-index parse possible
```

And for Earley/CYK:

```text
Selected backend:
  scalar_general_parser + optional simd lexer prefilter

Reason:
  ambiguous/general CFG; parser algorithm dominates
```

No silent “SIMD” marketing label. The compiler should say what it actually optimized.

## What “on par with dedicated JSON libraries” could mean

For a **literal JSON grammar**, compiled with a **tape/event output mode**, I think it is plausible to get into the same broad class as dedicated libraries, especially if the compiler recognizes the JSON shape and emits a structural-index path.

But there are three caveats:

First, simdjson is not just “SIMD plus parser.” It is a hand-tuned JSON architecture: structural index, string handling, UTF-8 concerns, number parsing, tape layout, and years of micro-optimizations. A generic grammar compiler can copy the architecture for eligible grammars, but it will not automatically match every hand-tuned corner.

Second, full Lark semantics are expensive. If you demand Python-Lark-compatible `Tree(Token(...))` output with metadata, callbacks, placeholders, filtered punctuation, aliases, and recovery behavior, you have already chosen more work than a dedicated JSON DOM/tape parser.

Third, many grammars do not have JSON’s clean structural alphabet. A language grammar with identifiers, keywords, strings, comments, indentation, contextual terminals, and many operators will benefit from SIMD lexing and allocation cuts, but not from a neat structural-index parse.

So my realistic expectation would be:

| Grammar/output class | Expected result |
|---|---|
| JSON grammar + tape/event output | Potentially close to dedicated JSON parsers; maybe within a small constant factor if heavily tuned |
| JSON grammar + full Lark tree | Faster than generic Lark path, but not simdjson-class |
| Programming-language LALR grammar | Good gains from SIMD lexing, fusion, span tokens, arena trees; not JSON-library throughput |
| Earley/CYK/general ambiguous grammar | SIMD helps scanning only; parser algorithm dominates |
| Arbitrary grammar with callbacks/postlex/dynamic lexer | Needs fallback or partial acceleration |

## The implementation path I would take

**Phase 1: finish bakeability.** Serialize the existing DFA scanner bundle into standalone Rust, then make `include_lark!`, C, and WebAssembly use the same baked data. This turns the current DFA work into a code-generation substrate.

**Phase 2: generate byte-class plans.** Extend scanner-plan construction with `ByteClassPlan`: start bytes per terminal set, ignored-run classes, newline masks, delimiter masks, quote/escape masks, and scalar fallback. Add deterministic tests that prove the SIMD classifier never skips a possible token start.

**Phase 3: add SIMD prefilter and skip loops.** Implement `x86_64` AVX2/SSE2, `aarch64` NEON, and scalar fallback behind one API. Start with whitespace/comment skipping and “next possible token start”. This is low-risk and useful for many grammars.

**Phase 4: specialize JSON-like structural grammars.** Detect a small structural alphabet and generate a structural-index parser. JSON should be the first proof target, because the correctness oracle and benchmark story are obvious.

**Phase 5: fuse generated LALR lexing and parsing.** Emit state-specialized code for contextual LALR: valid-terminal byte classes per state, direct shifts of spans, reductions without intermediate token allocation when possible.

**Phase 6: add output backends.** Full tree stays the compatibility target. Span tree, arena tree, event mode, and tape mode become speed targets. Without this, SIMD will hit the allocation wall.

**Phase 7: benchmark against both Python Lark and dedicated parsers.** Keep the current oracle-first discipline: scalar vs SIMD differential tests, mutation tests for skipped-start bugs, and perf counters for bytes scanned, DFA calls, allocations, and reductions. The repo already has the right culture for this: Python Lark as oracle, deterministic scaling counters, and recorded trend benchmarks. [filecite: turn31file0]

## The blunt answer

**Yes, it is possible to build a generic grammar compiler that emits SIMD-accelerated parsers.** For `okalldal/lark`, the current Rust architecture is unusually well-positioned because the DFA scanner, contextual lexer, interned symbols, dense parse tables, and oracle discipline are already there.

**No, it is not possible to honestly promise simdjson-class performance for arbitrary grammars.** That is not a SIMD problem; it is a grammar/output-semantics problem.

The winning product would be:

> “Compile any Lark grammar. Preserve semantics. When the grammar proves eligible, emit progressively faster generated backends — SIMD scanner, fused LALR, span/arena tree, structural-index tape — and explain exactly which tier was selected.”

That would be a serious parser-generator project, not a patch. But the repo has already done enough of the hard groundwork that this is a credible next architecture layer rather than science fiction.

---

**5. User** · `2026-06-21T06:53:28+02:00`

Hmm, what is it called in the parser world where you bolt on an ”output format” to the parser? Ie, the output most people want is not the lark tree node data structures but rather their own application specific data structures. Is there any features for this in lark and lark-rs and is this also an area where gains could be had to ultra speeds?

---

**6. Assistant** · `2026-06-21T06:53:42+02:00` · `gpt-5-5-pro`

I’ll pin down the parser terminology and map it to what Lark and lark-rs actually expose. The key performance question is whether they can construct application data directly instead of building a generic tree first.

---

**7. Assistant** · `2026-06-21T06:56:26+02:00` · `gpt-5-5-pro`

In parser terminology, the thing you mean is usually called **semantic actions** or **syntax-directed translation**.

More precise names by style:

| What you mean | Common name |
|---|---|
| Code attached to grammar productions that builds app objects | **Semantic actions** / **reduce actions** |
| The values carried on the parse stack | **Semantic values** |
| A formal model where grammar rules compute values | **Syntax-directed translation** or **attribute grammar** |
| Build a parse tree first, then convert it | **Tree transformation**, **visitor**, **AST lowering** |
| Convert parse events to another representation | **Listener**, **event sink**, **parse transducer** |
| Replace generic tree output with typed app structs | **AST builder** or **semantic builder** |

For your framing, I would call the feature in `lark-rs` something like **semantic output backends** or **generated semantic actions**. “Output format” sounds too much like serializing a tree to JSON/XML. The high-performance version is not serialization; it is **never building the generic tree in the first place**.

## What Python Lark already has

Python Lark has this pretty well covered at the library level.

It has `Transformer`, `Visitor`, and `Interpreter`. The docs say they process the parse trees Lark returns, with methods corresponding to rule names; `Transformer` replaces nodes with the returned value from each method. [filecite: turn39file0]

More importantly, Lark has an embedded `transformer=` option. The option docs say it applies the transformer to every parse tree and is equivalent to applying it after parse, but faster. [filecite: turn40file0] Internally, Lark builds parser callbacks from the transformer, so rule reductions can call the user’s transformer method instead of always constructing a plain `Tree`. The `ParseTreeBuilder.create_callback()` code looks up methods by rule name or alias and installs them as callbacks. [filecite: turn49file0]

The big limitation: embedded transformers are not allowed with Earley. The code explicitly rejects `transformer=` when `parser='earley'`; you have to parse first and transform afterwards. [filecite: turn41file0]

Python Lark also has smaller output-shaping knobs:

`tree_class` lets you replace the default `Tree` class. `keep_all_tokens`, `maybe_placeholders`, aliases, `_rule`, `?rule`, and token filtering shape the resulting tree. The options docs mention `tree_class`, token retention, placeholders, and lexer callbacks. [filecite: turn40file0] The parse tree builder implements the usual Lark shaping: filter punctuation, expand transparent rules, insert placeholders, propagate positions, and apply transformer callbacks. [filecite: turn48file0]

So in Python Lark terms, the feature is mostly: **Transformer**, especially **embedded transformer** for Look-Ahead Left-to-Right (LALR).

## What lark-rs currently has

`lark-rs` currently does **not** appear to expose the equivalent of Python Lark’s `transformer=` or a user-defined semantic-action API.

The public `Lark::parse()` returns `ParseTree`, and `ParseTree` is either a `Tree` or a `Token`. The public `LarkOptions` fields include parser, lexer, ambiguity, position propagation, token retention, placeholders, strict mode, postlex, and lexer backend — but no transformer, visitor, semantic action, reducer, or custom tree class field. [filecite: turn45file0]

Internally, though, `lark-rs` has exactly the right seam: `TreeBuilder`. Its own comment says this is “the one place” where Lark’s tree-shaping semantics live, and explicitly notes that it is the single chokepoint where the node representation could later change, for example to an arena or interned labels. [filecite: turn36file0]

The current structures are still generic and allocation-heavy: `Token` owns `String` fields for type and value, `Tree` owns a `String` label and `Vec<Child>`, and `NodeValue` is an enum carrying token/tree/inline children through the parse stack. [filecite: turn35file0] [filecite: turn36file0]

That means `lark-rs` currently has the machinery to build Lark trees, but not yet the feature you are describing as a first-class output backend.

## Is this a major speed area?

Yes. Probably **more important than SIMD**, at least after the lexer is no longer terrible.

The repo’s own benchmark notes say the remaining LALR gap is expected headroom from deferred tree-representation work, and that parse throughput is allocation-bound, around **3 allocations per input byte**, not algorithm-bound. [filecite: turn30file0]

That is the smoking gun. If you want “ultra speeds,” generic tree construction is one of the first things to attack.

A normal parser pipeline often does this:

```text
bytes
  → tokens
  → generic parse tree
  → transformer walk
  → application data structure
```

A fast semantic-action pipeline does this:

```text
bytes
  → parser reductions
  → application data structure
```

That removes:

- most `Tree` allocations,
- most `Child` vectors,
- owned token-value strings,
- a second tree walk,
- generic labels stored as strings,
- punctuation tokens that are only needed to guide parsing,
- intermediate nodes from grammar mechanics.

For JSON, the difference is brutal. A full Lark tree for JSON is a lot of little heap objects. A fast JSON parser wants a tape, arena, event stream, or typed object builder. If `lark-rs` can compile a JSON grammar into direct semantic reductions, it can avoid building a “syntax tree about JSON” and instead build “the JSON value.”

## What the high-performance design should look like

I would not just add a Python-style `Transformer` clone. That is useful, but it leaves too much performance on the floor if implemented with dynamic dispatch and generic `Vec<NodeValue>` plumbing.

I would add tiers.

### Tier 1: post-parse transformer

This is the easy API:

```rust
let tree = parser.parse(input)?;
let ast = MyAst::from_lark_tree(tree)?;
```

Good ergonomics. Bad for ultra speed, because the generic tree is already built.

### Tier 2: embedded reducer callbacks

Equivalent to Python Lark’s embedded transformer:

```rust
trait Reducer<'i> {
    type Value;

    fn token(&mut self, terminal: SymbolId, span: std::ops::Range<usize>, input: &'i str) -> Self::Value;

    fn reduce(
        &mut self,
        rule: RuleId,
        children: &mut [Self::Value],
        span: std::ops::Range<usize>,
        input: &'i str,
    ) -> Self::Value;
}
```

The parser stack stores `Value` instead of `NodeValue`. Reductions call `reduce()`. Tokens are spans, not owned strings. This alone can cut a lot.

### Tier 3: generated typed semantic actions

For standalone/codegen, generate static calls:

```rust
fn reduce_add(lhs: Expr, _plus: (), rhs: Expr) -> Expr {
    Expr::Add(Box::new(lhs), Box::new(rhs))
}
```

or for JSON:

```rust
fn reduce_array(items: Vec<JsonValue>) -> JsonValue {
    JsonValue::Array(items)
}
```

The key is static dispatch and no generic tree. This is where Rust can beat Python Lark decisively.

### Tier 4: event/tape mode

For maximum speed, do not even build recursive app objects during parse:

```text
start_object
key("foo")
number(123)
end_object
```

or:

```text
TapeEntry::StartObject
TapeEntry::String(span)
TapeEntry::Number(span)
TapeEntry::EndObject
```

This is closer to simdjson’s model. It is the right target for “on par with dedicated JSON libraries.”

## The parser-world analogy

In Yacc/Bison, every production can have an action:

```yacc
expr:
    expr '+' term { $$ = Add($1, $3); }
  | term          { $$ = $1; }
```

Here, `$1`, `$3`, and `$$` are **semantic values**. The action is a **semantic action**. The whole thing is **syntax-directed translation**.

Python Lark’s `Transformer` is the Pythonic version of that. `lark-rs` could grow a Rust-native version that is both safer and faster.

## Where I would put it in lark-rs

The right seam is `TreeBuilder`.

Today:

```text
LALR reduce
  → collect NodeValue children
  → TreeBuilder::assemble()
  → NodeValue::Tree / Token / Inline
```

Future:

```text
LALR reduce
  → OutputBuilder::reduce(rule_id, child slice)
  → user/application value
```

Something like:

```rust
pub enum OutputMode {
    LarkTree,
    SpanTree,
    Events,
    Tape,
    CustomReducer,
}
```

or at compile/codegen level:

```rust
pub trait OutputBuilder<'i> {
    type Value;

    fn shift_token(
        &mut self,
        terminal: SymbolId,
        span: std::ops::Range<usize>,
        input: &'i str,
    ) -> Self::Value;

    fn reduce(
        &mut self,
        rule: usize,
        children: &mut Vec<Self::Value>,
        input: &'i str,
    ) -> Self::Value;
}
```

Then keep exact Lark compatibility as the default `TreeOutputBuilder`.

The important design rule: **tree compatibility is one output backend, not the parser’s identity**.

## The catch

Semantic actions can make parsers much faster, but they also hurt grammar portability if done badly.

Lark’s superpower is grammar-first parsing: the same grammar can run in LALR, Earley, CYK, Python, Rust, standalone, WebAssembly, etc. If semantic actions become inline Rust snippets in the grammar, that portability dies.

Better design:

- grammar stays pure,
- semantic mapping is separate,
- aliases/rule names are the stable hook points,
- generated standalone can bind to a Rust action module,
- fallback output remains the ordinary Lark tree,
- oracle tests still compare tree mode against Python Lark.

So I would avoid embedding raw Rust in `.lark` files. Use a generated trait/interface keyed by rule aliases and terminals.

## Bottom line

The parser-world name is **semantic actions**; the formal term is **syntax-directed translation**; the Lark feature name is **Transformer**.

Python Lark already has this, including a faster embedded transformer path for LALR. `lark-rs` currently does not expose it, but its `TreeBuilder` is exactly the seam where it should be added.

And yes: this is absolutely an ultra-speed area. For many grammars, especially JSON-like ones, direct semantic output could matter more than SIMD. SIMD helps find tokens faster; semantic output avoids building millions of tiny generic objects you never wanted.

---

**8. User** · `2026-06-21T07:12:02+02:00`

Suppose we wanted to add the semantic actions/syntax-directed translation/ Lark 
Transformer functionality to lark-rs, with the goal of enabling ultra speed embedding eventually, would it be possible to work oracle driven in this? Per the repo philosophy, and with the pupfish achieving autonomous independent agent implementation of as much as possible, minimising the need for human oversight and course setting/course correction. What are the critical places where oracle testing is not solid and where can we add it?

---

**9. Assistant** · `2026-06-21T07:12:17+02:00` · `gpt-5-5-pro`

I’ll frame this as an oracle-driven implementation plan for `lark-rs`: which transformer semantics can be proven externally, where direct semantic output risks self-verification, and what tests close those gaps.

---

**10. Assistant** · `2026-06-21T07:15:20+02:00` · `gpt-5-5-pro`

The current oracle mostly freezes tree shape and errors, not semantic values, callback order, token-callback effects, spans, or allocation behavior. I’ll separate correctness oracles from performance counters.

---

**11. Assistant** · `2026-06-21T07:16:17+02:00` · `gpt-5-5-pro`

Yes. This is one of the best places to apply the repo’s oracle-first philosophy, **provided the first milestone is semantic parity, not speed**.

The right framing:

> Add an `OutputBuilder` / semantic-action seam to `lark-rs`, with the existing Lark `Tree` builder as one backend. Then add a Python-Lark-transformer-parity backend. Only after that add span/arena/event/tape backends for speed.

The repo already says the safe autonomy boundary is “what we have made falsifiable,” and that Python Lark is the oracle for expected trees/errors generated through `tools/generate_oracles.py`. [filecite: turn52file0] The current oracle generator serializes Python Lark parse trees to committed JSON, and Rust tests compare `ParseTree` recursively against those oracle files. [filecite: turn50file0] [filecite: turn51file0] That pattern can absolutely be extended to transformers.

## The good news

Python Lark gives a strong oracle for the **LALR transformer feature**. Its `transformer=` option says it applies the transformer during parse, equivalent to applying it after parse but faster. [filecite: turn40file0] Internally, Python Lark’s `ParseTreeBuilder.create_callback()` maps rules/aliases to transformer methods and installs those callbacks as parser callbacks. [filecite: turn49file0]

`lark-rs` also has the right implementation seam already: `TreeBuilder`. Its comment explicitly says tree shaping lives in one place and that this is the chokepoint where the node representation could later change, including arena or interned-label representations. [filecite: turn36file0]

So the feature does not need to start by rewriting the parser. It starts by replacing:

```text
reduce(rule) → TreeBuilder::assemble(...) → NodeValue::Tree / Token / Inline
```

with:

```text
reduce(rule) → OutputBuilder::reduce(...) → user/output value
```

and keeping the current tree behavior as the default output builder.

## The bad news

The oracle is not equally strong for every part of the eventual “ultra speed embedding” story.

Python Lark can oracle **semantic results**. It cannot directly oracle:

- Rust-specific typed structs,
- zero-copy span lifetimes,
- allocation counts,
- event streaming behavior,
- tape layout,
- exact callback side-effect order unless we explicitly record traces,
- future SIMD/fused-parser fast paths,
- arbitrary user-written Rust semantic code.

Those need extra gates.

So the strategy should be: **use Python Lark for semantic parity; use deterministic Rust counters and metamorphic tests for the speed representations.**

## What to build first

### 1. Add transformer oracles, not the feature

Create something like:

```text
lark-rs/tools/generate_transformer_oracles.py
lark-rs/tests/fixtures/transformers/*.json
lark-rs/tests/test_transformer_oracle.rs
```

The generator should run Python Lark with a set of small deterministic transformers and serialize the transformed result as JSON.

Do not use arbitrary Python snippets as the long-term oracle format. Use a small action-spec format that both Python and Rust can implement.

Example fixture shape:

```json
{
  "name": "json_value_builder",
  "grammar": "json.lark",
  "parser": "lalr",
  "lexer": "contextual",
  "transformer": "json_value_v1",
  "cases": [
    {
      "input": "{\"a\": [1, true, null]}",
      "ok": true,
      "value": {
        "object": {
          "a": [{ "number": "1" }, true, null]
        }
      }
    }
  ]
}
```

That gives agents a falsifiable target before they touch the Rust parser.

### 2. Add a Rust `OutputBuilder` behind the existing tree path

The existing tree builder must become one implementation:

```rust
trait OutputBuilder<'i> {
    type Value;

    fn token(
        &mut self,
        terminal: SymbolId,
        span: std::ops::Range<usize>,
        input: &'i str,
    ) -> Self::Value;

    fn reduce(
        &mut self,
        rule: usize,
        children: &mut Vec<Self::Value>,
        input: &'i str,
    ) -> Self::Value;
}
```

The compatibility backend returns the current `NodeValue`. A semantic backend returns fixture JSON values or typed values.

Current `lark-rs` publicly returns `ParseTree` from `Lark::parse`, and its options do not expose a transformer/output hook today. [filecite: turn45file0] Tokens and trees currently own `String` data and `Vec<Child>` nodes, which is exactly the allocation-heavy path the speed work should eventually bypass. [filecite: turn35file0]

### 3. First target: pure LALR transformer parity

Start with LALR + contextual lexer only. That matches Python Lark’s embedded-transformer support. Python Lark explicitly rejects embedded transformers with Earley, so Earley must not be in the first autonomous implementation target. [filecite: turn41file0]

Done-when should be brutally concrete:

```text
For each transformer fixture:
  Python Lark transformer result JSON == lark-rs semantic output JSON
```

No human judgement required.

## Critical weak oracle spots and how to fix them

| Weak spot | Why the oracle is weak | Gate to add |
|---|---|---|
| Arbitrary user Rust actions | Python cannot know what a Rust closure/trait implementation “should” do | Use paired action specs for oracle fixtures. Arbitrary user code is tested by user tests, not Python parity |
| Public API shape | Python Lark cannot decide Rust ergonomics, lifetimes, trait names, or generic design | Architect decision or Architecture Decision Record (ADR). This is `escalate`, not autonomous |
| Embedded Earley transformer | Python Lark rejects `transformer=` with Earley | Defer. Later compare post-parse transform results only, or explicitly define Rust as an extension with an ADR |
| Ambiguity handling | Transformers over `_ambig` trees are oracleable after parse, but reduction-time semantic actions over a forest are not straightforward | Keep first transformer feature LALR-only. Add separate explicit-ambiguity post-transform fixtures later |
| `v_args(meta=True)` / meta arguments | Python Lark’s internal transformer wrapper rejects meta args for embedded transformers; it says to transform after parse instead | Initial scope: no embedded meta args. Later add post-parse meta oracle with `propagate_positions=True` |
| Side effects and callback order | A pure result oracle misses whether callbacks fire in the same order | Add a trace transformer oracle: every rule/token callback appends `{kind, name, children_repr}` to a list |
| Token transformer methods | Python Lark wires transformer terminal methods into lexer callbacks via `_get_lexer_callbacks`, not just rule reductions | Add fixtures for terminal callbacks: token conversion, keyword retyping, ignored-token exclusion, callback error |
| `Discard` behavior | Dropping values can change child positions and placeholder behavior | Add dedicated `Discard` fixtures for tokens and rules |
| Aliases/templates | Callback name is alias or template source or rule origin name in Python Lark | Add fixtures for `-> alias`, template-expanded rules, and same-origin multiple alternatives |
| `_rule`, `?rule`, punctuation filtering | Existing tree oracle covers tree shape, not transformed values | Add transformer fixtures for transparent rules, expand-one, filtered punctuation, `keep_all_tokens`, and `maybe_placeholders` |
| Position/span output | Python Lark gives token values and positions, not Rust lifetimes/spans | Add metamorphic gates: every emitted span slices back to the exact Python token value; materialized span output equals transformer oracle |
| Error/recovery interaction | Transformer behavior on partial/error recovery is subtle and likely not covered | Initial scope: no transformer on recovery. Later add explicit `parse_on_error + transformer` parity fixtures |
| Performance claims | Python Lark cannot oracle allocation count or speed | Add deterministic counters, not wall-clock: tree nodes built, token strings allocated, child vectors allocated, semantic reductions, value clones |
| Standalone output | Current standalone runtime lexes into `Vec<Token>` and builds `Tree`; transformer output is absent | Add standalone transformer fixtures and compare generated parser output to the same transformer oracle |
| Bindings: PyO3/WebAssembly/C | Python Lark can oracle semantics, not foreign-function-interface shape | Semantic parity tests plus binding smoke tests; public binding design is architect-level |

## The biggest missing gate: callback trace parity

A result-only oracle is not enough for autonomous work. It will miss order bugs and accidental skipped callbacks.

Add a Python transformer like this conceptually:

```python
class Trace(Transformer):
    def __default__(self, data, children, meta):
        trace.append(["rule", data, repr(children)])
        return {"rule": data, "children": children}

    def __default_token__(self, token):
        trace.append(["token", token.type, str(token)])
        return {"token": token.type, "value": str(token)}
```

Then serialize:

```json
{
  "value": "...final transformed result...",
  "trace": [
    ["token", "NUMBER", "1"],
    ["rule", "atom", "..."],
    ["rule", "expr", "..."]
  ]
}
```

Rust should have a matching `TraceOutputBuilder`.

This turns “did we really implement Python Lark transformer semantics?” into a hard test rather than a reviewer vibe.

## The second missing gate: allocation/work counters

The repo already has a rule that performance claims should use deterministic counters, not wall-clock. [filecite: turn52file0] Extend `perf-counters` with output counters:

```text
tree_nodes_built
tokens_materialized
token_value_string_bytes
tree_label_string_bytes
child_vec_allocs
nodevalue_vec_drains
semantic_reduce_calls
semantic_value_clones
span_tokens_emitted
```

Then add tests like:

```text
Tree output:
  tree_nodes_built > 0
  token_value_string_bytes > 0

Semantic JSON output:
  tree_nodes_built == 0
  child_vec_allocs == 0 or bounded
  token_value_string_bytes == 0 for span-preserving actions
  semantic_reduce_calls == number_of_reductions
```

This is where the “ultra speed” path becomes falsifiable. The oracle says the result is right; counters say the fast path is actually fast-shaped.

## Suggested autonomous implementation slices

These are good agent-sized tasks with minimal course correction.

### Slice 1: transformer oracle generator

Done-when:

```text
tools/generate_transformer_oracles.py produces committed JSON for:
  arithmetic evaluator
  JSON value builder
  token-normalizer
  alias/expand1/transparent fixture
  maybe_placeholders fixture
  keep_all_tokens fixture
  Discard fixture
  trace fixture
```

No Rust parser changes yet.

### Slice 2: output-builder refactor with no behavior change

Done-when:

```text
Existing tree oracle bank remains green.
No public API change.
TreeBuilder becomes TreeOutputBuilder or equivalent.
```

This is a safe refactor if the banks stay green.

### Slice 3: internal semantic-output test backend

Done-when:

```text
Rust TraceOutputBuilder matches transformer trace oracle.
Rust JsonFixtureOutputBuilder matches transformer value oracle.
Only tests use it; public API still unchanged.
```

This avoids premature API bikeshedding.

### Slice 4: public LALR transformer API

This is **not** fully autonomous. It changes public API and Rust ergonomics. Per the repo’s principles, new public API and architecture changes are escalate-tier. [filecite: turn52file0]

The implementation can be agent-driven, but the trait shape should be an architect decision.

### Slice 5: standalone semantic output

Current standalone runtime bakes grammar data, lexes into `Vec<Token>`, then builds `Tree`/`ParseTree`. [filecite: turn46file0] [filecite: turn47file0]

Done-when:

```text
Generated standalone parser can run the same transformer fixture backend.
Standalone output JSON == transformer oracle JSON.
Tree standalone remains unchanged.
```

### Slice 6: no-tree fast path

Done-when:

```text
Semantic output fixtures pass.
Counters prove zero Tree construction.
Counters prove zero owned token-value strings for span-only semantic actions.
```

This is the first genuinely speed-relevant slice.

## What should be escalated to the architect

Agents can implement most of this once the gates exist. But these questions should not be guessed:

1. **Public API shape:** trait-based reducer, generated trait, closures, or proc-macro binding?
2. **Whether semantic mappings live in `.lark` files:** I would say no. Keep grammars pure; bind actions externally.
3. **Initial support boundary:** LALR-only? I would say yes.
4. **Side effects:** are they supported as Python-compatible behavior, or only pure semantic builders? I would support trace parity, but document that ultra-speed builders should be pure.
5. **Meta/span policy:** owned strings first, spans later? I would design spans from day one internally, even if public API starts owned.
6. **Output modes:** `Tree`, `SpanTree`, `Event`, `Tape`, `Custom`. That is product direction, not just implementation.

## The core test matrix to add

Minimum serious matrix:

```text
Parser:
  LALR + basic lexer
  LALR + contextual lexer

Tree shaping:
  normal rule
  alias
  template source
  _transparent rule
  ?expand1 rule
  punctuation filtering
  keep_all_tokens
  maybe_placeholders true/false
  empty rule
  terminal callback
  Discard
  default rule handler
  default token handler

Integration:
  %ignore
  unless keyword retyping
  postlex Indenter, later
  standalone, later

Comparison:
  transformed value equality
  callback trace equality
  materialized span equality
  tree-mode regression equality
  deterministic output counters
```

## Bottom line

Yes, this can be made very oracle-driven.

The strong path is:

1. Generate Python Lark transformer result and trace oracles.
2. Refactor `TreeBuilder` into an output-builder seam without changing behavior.
3. Add internal semantic builders that match those oracles.
4. Only then expose a public Rust semantic-action API.
5. Add deterministic counters proving the fast path avoids generic tree allocation.

The weak spots are not mysterious: arbitrary user code, public API design, Earley/ambiguity, meta args, side-effect order, span lifetimes, recovery, standalone/bindings, and performance claims. Most can be turned into gates. The few that cannot are exactly the ones that should become ADRs or architect decisions.

---

**12. User** · `2026-06-21T07:32:44+02:00`

What’s the way to making all of this happening using the machinery/process in the repo?

---

**13. Assistant** · `2026-06-21T07:32:59+02:00` · `gpt-5-5-pro`

I’ll map this onto the repo’s actual operating model: decision record and issue contract first, then oracle generation, gated autonomous slices, and escalation only for API or architecture choices.

---

**14. Assistant** · `2026-06-21T07:35:47+02:00` · `gpt-5-5-pro`

The way is: **turn this into a gated phase, not a coding task**.

Use the repo’s machinery exactly as intended:

```text
/roadmap → architect approves direction
  → tracking issue + decision issues + autonomous child issues
  → /triage apply
  → /next-task for one-at-a-time work
  → /start-sprint only after the backlog is fully triaged and groundable
  → /finish-task for normal child work
  → /review-pr auto-merges only fully gated auto-tier PRs
  → architect merges API/architecture/escalate-tier work
```

The key move is to make the first deliverable **the oracle harness**, not the Rust API. Once the gates exist, most of the implementation can be done by autonomous agents with much less steering.

## The repo-native plan

### 1. Run this through `/roadmap`, not `/next-task`

This is a new capability area: semantic actions / transformer parity / fast output backends. The repo says `/roadmap` is the proactive planning layer: it surveys project state, proposes epics/issues, and brings them to the architect; it explicitly **does not execute or file unapproved issues**. [filecite: turn54file0]

So the first act should be a `/roadmap` proposal for a new epic, probably:

```text
Epic: Phase 5 — semantic output builders and transformer parity
```

or, less phase-heavy:

```text
Epic: semantic output backends
```

The proposal should include:

```text
Theme:
  Add syntax-directed translation / Transformer-equivalent output backends to lark-rs.

Why now:
  Existing TreeBuilder is the single output-shaping seam.
  Current LALR speed is allocation-bound.
  This unlocks future zero-tree / span / event / tape modes.

Grounding:
  Python Lark transformer result oracles + callback trace oracles.
  Existing tree oracles stay green.
  perf-counters prove no-tree fast path shape.

Blast radius:
  parsers/tree_builder.rs, parsers/lalr.rs, tree.rs, tools/generate_*oracles.py,
  tests/fixtures/oracles, standalone runtime later.
```

Do **not** let an agent start coding from this chat. Capture the direction durably in GitHub. `/roadmap`’s own command says chat approval must be recorded in a tracking issue or Architecture Decision Record (ADR) before child issues are filed. [filecite: turn54file0]

## 2. Split it into one decision issue and many autonomous issues

The public API and architecture are not auto-tier work. The repo’s Definition of Done and merge-tier rules say public API, new grammar semantics, architecture, and governance/policy changes are `escalate` and require architect merge; fully oracle-backed bugfixes/refactors/perf fixes can be `auto`. [filecite: turn57file0]

So create one `needs-decision` issue up front, then keep the rest small and oracle-backed.

### Decision issue: API and architecture boundary

Suggested title:

```text
Decision: semantic output builder API for lark-rs
```

Labels:

```text
lark-rs
kind:feat
prio:now
needs-decision
semantic-output
```

Done-when:

```text
An accepted ADR defines:
- whether the public API is trait-based, generated-trait-based, closure-based, or staged/internal first;
- whether semantic mappings are external Rust code, never inline .lark code;
- initial support boundary: LALR-only vs broader;
- span policy: owned strings first vs internal spans from day one;
- whether transformer parity includes side-effect order as a public guarantee;
- how standalone/PyO3/WASM/C are staged.
```

Recommended decision:

```text
Keep grammars pure. Add an internal OutputBuilder seam first. Public API comes after the oracle harness proves parity. Initial support is LALR-only. Internal token representation should use spans from day one, even if public fixtures materialize strings.
```

This one is architect territory. Everything below can be made `good-autonomous`.

## 3. File autonomous child issues with hard done-whens

The label schema already defines `good-autonomous` as “fully groundable, safe for unattended `/next-task` pick,” and `needs-decision` as the architect’s inbox. `/next-task` prefers `good-autonomous + prio:now` and skips `needs-decision`. [filecite: turn53file0] [filecite: turn55file0]

Here is the backlog I’d file.

### Issue 1: transformer oracle generator

Title:

```text
Add Python Lark transformer oracle generator
```

Labels:

```text
lark-rs
testing
kind:feat
prio:now
good-autonomous
semantic-output
```

Files:

```text
lark-rs/tools/generate_transformer_oracles.py
lark-rs/tests/fixtures/oracles/transformer/
lark-rs/tests/common/
```

Done-when:

```text
A new generator produces committed transformer oracle JSON from Python Lark for:
- arithmetic evaluator;
- JSON value builder;
- token-normalizer;
- alias callbacks;
- _transparent rule shaping;
- ?expand1 shaping;
- filtered punctuation;
- keep_all_tokens;
- maybe_placeholders true/false;
- Discard;
- default rule handler;
- default token handler;
- callback trace order.

The generated files are consumed by at least one Rust test that loads and validates the fixture schema, even if lark-rs does not yet implement semantic output.
```

Why first: it creates the falsifiable surface. No Rust design bikeshedding required.

### Issue 2: callback trace oracle

This can be folded into issue 1, but I would split it because it is the gate that prevents fake parity.

Title:

```text
Add transformer callback trace oracles
```

Done-when:

```text
Python Lark oracle fixtures include both:
- final transformed value;
- ordered callback trace for rules and tokens.

Trace fixtures cover aliases, token callbacks, default handlers, Discard, and filtered tokens.
```

Why separate: result equality can miss callback order bugs. Trace equality makes the behavior agent-checkable.

### Issue 3: refactor TreeBuilder behind an output seam, no behavior change

Title:

```text
Refactor TreeBuilder into the default output backend
```

Labels:

```text
kind:refactor
prio:now
good-autonomous
semantic-output
```

Files:

```text
lark-rs/src/parsers/tree_builder.rs
lark-rs/src/parsers/lalr.rs
lark-rs/src/tree.rs
```

Done-when:

```text
The existing public parse API still returns the same ParseTree.
All existing tree oracle tests pass unchanged.
No public API is added.
TreeBuilder becomes the default implementation of an internal output-building seam, or an equivalent preparatory refactor.
```

This is safe because the current oracles already compare Rust parse trees against Python Lark output recursively. [filecite: turn51file0]

### Issue 4: internal fixture semantic backend

Title:

```text
Implement internal transformer-fixture output backend for LALR
```

Labels:

```text
kind:feat
prio:now
good-autonomous
semantic-output
```

Done-when:

```text
A test-only semantic backend can run the transformer oracle fixtures.
For every fixture:
  Rust semantic output JSON == Python Lark transformer value JSON
  Rust callback trace JSON == Python Lark callback trace JSON

Scope:
  LALR + basic lexer
  LALR + contextual lexer
  no public API
  no Earley/CYK
```

This is the first actual syntax-directed translation implementation, but still safely internal.

### Issue 5: token callbacks and lexer callback parity

Title:

```text
Pin transformer terminal callback parity
```

Done-when:

```text
Transformer oracle fixtures prove terminal callbacks match Python Lark for:
- converting token values;
- default token handling;
- ignored tokens not surfacing;
- keyword/identifier unless retyping;
- token callback trace order.

Rust internal semantic backend matches all fixtures.
```

Why important: Python Lark wires transformer terminal methods into lexer callbacks, not only parser reductions. This is a sharp edge.

### Issue 6: Discard and child-shaping parity

Title:

```text
Pin Discard and child-shaping parity for semantic output
```

Done-when:

```text
Semantic output fixtures cover:
- Discard from token callbacks;
- Discard from rule callbacks;
- punctuation filtering;
- transparent rule splicing;
- expand1;
- placeholders;
- aliases.

Rust output and trace match Python Lark.
```

This prevents “works on arithmetic but wrong for real Lark semantics.”

### Issue 7: perf counters for output modes

Title:

```text
Add deterministic output-shape perf counters
```

Labels:

```text
kind:perf
prio:now
good-autonomous
performance
semantic-output
```

Files:

```text
lark-rs/src/perf.rs
lark-rs/tests/test_*scaling*.rs
lark-rs/BENCH.md
```

Done-when:

```text
Under --features perf-counters, lark-rs records:
- tree_nodes_built;
- token_value_string_bytes;
- tree_label_string_bytes;
- child_vec_allocs or child_vec_builds;
- semantic_reduce_calls;
- semantic_token_calls;
- semantic_value_clones where measurable.

A test proves:
- normal Tree output builds trees;
- semantic fixture backend does not build Tree nodes;
- span-only fixture path does not allocate token value strings, once span mode exists.
```

This matches the repo’s rule that performance claims must be deterministic and counter-based, not wall-clock. [filecite: turn52file0]

### Issue 8: public API ADR implementation

This one should be blocked on the decision issue.

Title:

```text
Expose LALR semantic output builder API
```

Labels:

```text
kind:feat
prio:next
needs-decision until ADR accepted
semantic-output
```

Done-when:

```text
The accepted semantic-output ADR is implemented.
Public docs/examples show:
- parse to normal Lark tree;
- parse with custom semantic builder;
- parse with span-preserving builder, if included in ADR scope.

Transformer oracle fixtures still pass through the public API path.
```

This should not be `good-autonomous` until the ADR is accepted.

### Issue 9: standalone semantic output

Title:

```text
Support semantic output in generated standalone parsers
```

Labels:

```text
kind:feat
prio:next
good-autonomous after public/internal seam settles
distribution
semantic-output
```

Why later: current standalone runtime bakes its own `Token`, `Tree`, `Child`, `NodeValue`, lexes into `Vec<Token>`, and builds trees. [filecite: turn46file0] [filecite: turn47file0] It will need a parallel output seam after the main crate has one.

Done-when:

```text
A generated standalone parser can run a fixture semantic backend.
Standalone semantic output JSON == Python Lark transformer oracle JSON.
Existing standalone tree output remains byte-for-byte compatible with current tests.
```

### Issue 10: fast span/event/tape backends

These are later, after semantic parity.

Title:

```text
Add zero-tree span/event output backend
```

Done-when:

```text
Semantic result matches transformer oracle after materializing spans.
perf-counters prove:
- no Tree nodes;
- no owned token value strings;
- bounded child buffer reuse;
- one semantic reduction per parser reduction.
```

Title:

```text
Add JSON tape output backend
```

Done-when:

```text
JSON grammar parses into a tape/event representation.
Materialized tape equals Python Lark JSON transformer oracle.
Counters prove no generic tree construction.
Bench trend records speed delta, but CI gates only counters and correctness.
```

## 4. Run `/triage`, then `/triage apply`

After issues are drafted, run `/triage` in dry-run mode. The command is explicitly report-only by default because labels drive autonomy and merge-tiering; only `/triage apply` mutates after architect approval. [filecite: turn56file0]

The intended classification should look like:

```text
needs-decision:
  API/ADR issue

good-autonomous + prio:now:
  transformer oracle generator
  trace oracle
  TreeBuilder no-op refactor
  internal semantic backend
  token callback parity
  Discard/child shaping parity
  output perf counters

prio:next:
  public API implementation
  standalone
  span/event/tape fast paths
```

This matters because `/next-task` and `/start-sprint` read labels; they do not infer your intent from this conversation.

## 5. Use `/next-task` until the gates are in place

For the first few issues, do **not** start a whole sprint. Use `/next-task`.

`/next-task` surveys open PRs/issues, prefers `good-autonomous + prio:now`, skips `needs-decision`, claims one issue, implements with failing oracle/test first, runs `/finish-task`, and opens a PR. [filecite: turn55file0]

The first autonomous order should be:

```text
1. transformer oracle generator
2. callback trace oracle
3. TreeBuilder no-op refactor
4. internal semantic backend
5. token callback parity
6. Discard/child shaping parity
7. output perf counters
```

After issue 1 and issue 2 land, the rest becomes much safer. Before that, agents will be inventing semantics.

## 6. Use `/finish-task` for each normal issue

The end-of-task loop is already right:

```text
/code-review
scripts/check-fast.sh
open PR
CI full gate on pull_request
file follow-ups
ADR if architecture/public API decision
classify auto vs escalate
```

The command says local fast gate is `lark-rs/scripts/check-fast.sh`, and full CI runs on the pull request, including the broader gates. It also says generator/oracle changes should run the oracle freshness step to avoid stale-oracle CI failures. [filecite: turn57file0]

For semantic-output work, every PR description should include:

```text
Failed-first evidence:
  which oracle/test failed before implementation

Now passes:
  exact cargo test command
  oracle regen/freshness if touched

Scope:
  no public API, or ADR-linked public API

Follow-ups:
  filed issue numbers
```

That makes `/review-pr` able to judge without reconstructing the session.

## 7. Only use `/start-sprint` after the backlog is shaped

Once the oracle generator and trace gates exist, this becomes a good sprint candidate. `/start-sprint` is designed to run all schedulable open issues onto an integration branch, with worker sub-agents opening child PRs and a final architect-approved omnibus PR. It requires a green base, accepted ADR preconditions, and a fully triaged backlog; it refuses untriaged or decision-blocked work. [filecite: turn58file0]

Use it only when most issues are:

```text
good-autonomous
not needs-decision
not blocked
clear Done-when
clear Files/blast-radius
```

This is perfect for a phase like:

```text
semantic-output parity hardening sprint
```

It is not suitable for the initial API decision.

## 8. Keep the architect’s role narrow

The architect should decide only:

```text
1. Are we doing this epic now?
2. What is the public API / ADR boundary?
3. What output modes are product commitments vs experiments?
4. When does the sprint omnibus merge?
```

Agents can decide:

```text
- fixture implementation details;
- oracle generator layout;
- internal refactors preserving tree output;
- parity bug fixes;
- perf counter implementation;
- standalone parity once the seam exists.
```

That matches the repo constitution: agents may decide anything grounded by an oracle/gate/bank; judgment-only product/API choices go to the architect or an ADR. [filecite: turn52file0]

## The crisp execution sequence

```text
1. /roadmap
   Propose "semantic output builders" epic.

2. Architect approves direction.

3. File durable tracking issue:
   "Epic: semantic output builders and Transformer parity"

4. File one needs-decision issue:
   "Decision: semantic output builder API"

5. File good-autonomous child issues:
   oracle generator, trace oracle, no-op output seam refactor,
   internal semantic backend, token callbacks, Discard/shaping, perf counters.

6. /triage
   Dry-run labels.

7. Architect approves triage.

8. /triage apply

9. /next-task repeatedly until oracle gates and internal seam land.

10. Write/accept ADR for public API.

11. Convert public API implementation issue from needs-decision to good-autonomous or keep escalate-tier.

12. Use /start-sprint for the remaining fully groundable backlog.

13. Architect merges final sprint omnibus if used.
```

The most important guardrail: **never let “ultra speed embedding” be the first task**. First make Python Lark transformer semantics falsifiable. Then make the fast path prove it produces the same values while not building trees. Once those gates exist, the process can chew through a surprising amount of the work without needing you to babysit each turn.
---

## Source URL appendix

### Web citations with URLs
- `turn668427view0` — [Parsing Gigabytes of JSON per Second](https://arxiv.org/abs/1902.08318) (arXiv)
- `turn149936academia0` — [flap: A Deterministic Parser with Fused Lexing](https://arxiv.org/abs/2304.05276) (arXiv)

### Internal file citations without URLs in the archive

The shared ChatGPT archive preserved these as `filecite` handles with no `safe_urls` or `refs` metadata. The tool outputs that would have contained the underlying GitHub/file details were redacted in the shared conversation data, so these cannot be losslessly resolved from the `.webarchive` alone.

- `turn24file0`
- `turn21file0`
- `turn22file0`
- `turn33file0`
- `turn34file0`
- `turn26file0`
- `turn27file0`
- `turn30file0`
- `turn35file0`
- `turn36file0`
- `turn31file0`
- `turn39file0`
- `turn40file0`
- `turn49file0`
- `turn41file0`
- `turn48file0`
- `turn45file0`
- `turn52file0`
- `turn50file0`
- `turn51file0`
- `turn46file0`
- `turn47file0`
- `turn54file0`
- `turn57file0`
- `turn53file0`
- `turn55file0`
- `turn56file0`
- `turn58file0`

