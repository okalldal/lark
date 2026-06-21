# Spike — API design for #168 (interactive parser)

> **⚠️ Superseded / non-normative (2026-06-21).** Design *exploration*, not the
> shipped spec. The implementation landed in **PR #219** and differs deliberately:
> the v1 surface is the **oracle-backed subset** of Python's `InteractiveParser`
> **plus** a `feed(name, value)` convenience wrapper — *not* "exactly Python's ops,
> no extras", and `choices()` is **not** in v1 (deferred). Lexing is **lazy**. Where
> this spike disagrees with the shipped code or **ADR-0026**, those govern.

**Status:** design spike / decision-support for the architect. Escalate-tier (new
public API). Proposes a v1 surface, the refactor it needs, and — front and center
per the ask — **how it makes oracle testing trivial.** No code shipped.

**Design priorities, in order:**
1. **Oracle-testability** — the surface must map 1:1 onto Python Lark's
   `InteractiveParser` so a single test script drives both and compares.
2. **Usability** — idiomatic Rust; the common cases (recover, inspect, fork)
   should be a few lines, with no lifetime gymnastics.

The two priorities mostly *agree*, because "match Python's surface exactly" is
both what gives us the oracle and what gives us a surface real users already know
from Python Lark.

---

## 1. Why scope = Python's surface (and what that buys us)

Per the #211 resolution ("recovery scope = Python's scope"), #168's job is to
**port** Python Lark's `InteractiveParser`, not invent a richer one. That decision
is what makes #168 oracle-backed: every operation we expose has a Python
counterpart we can differentially test. The design rule that falls out:

> **Expose exactly Python's operations, named and shaped to match. Add no
> convenience op that Python lacks** — the moment we do, *that* op has no oracle
> and we're back in judgment-land (the "being more permissive is unfalsifiable"
> rule). Rust-idiomatic *spelling* is fine; new *capability* is not, in v1.

### Python's surface (the spec to match)

From `lark/parsers/lalr_interactive_parser.py` + `lalr_parser_state.py`:

| Python op | What it does | Oracle value |
|---|---|---|
| `feed_token(tok)` | run reduce+shift for one token; at `$END` in end-state, return the tree; on no-action raise `UnexpectedToken` | **high** — compare ok/raise + the resulting tree |
| `accepts()` | set of terminal *names* that would advance the parser (computed by trial-feeding a fork) | **highest** — pure sorted strings, fully deterministic, engine-agnostic |
| `choices()` | raw action-table row for the current state (token → Shift/Reduce) | high — typed map compare |
| `feed_eof(last)` | feed a synthetic `$END` | high — drives the finish |
| `exhaust_lexer()` | pull + feed the rest of the lexer (no `$END`); return tokens consumed | medium — token list compare |
| `resume_parse()` | finish fully-automatically from here | high — final tree compare (this is the recovery resume) |
| `copy()` / `__copy__` | fork an independent cursor | structural — exercised by `accepts()` and `match_examples` |
| `pretty()` | debug string of `choices()` + stack size | low — humans only |
| `__eq__` | equal on (state_stack length, position); used by `match_examples()` | medium — pin the equality contract |
| `.result` | tree once the parse is over | high |

`accepts()` is the crown jewel for oracle testing: it's a **set of strings**,
deterministic, and it changes after every `feed_token`, so a feed/inspect script
produces a rich, exactly-comparable trace with zero tree-shaping ambiguity.

---

## 2. What we have to build on (the internals)

Good news: the state machine is already factored to make this a *seam extraction*,
not a parallel engine (ADR-0015 — consolidate seams before features).

Today `LalrParser::run` and `run_recovering` (`src/parsers/lalr.rs`) each keep two
locals — `state_stack: Vec<usize>` and `value_stack: Vec<NodeValue>` — and inline
the same SHIFT/REDUCE/ACCEPT match. Python's structure is the same two stacks,
just lifted into a `ParserState` object that `InteractiveParser` wraps. The lexer
side is already abstracted behind the `TokenSource` trait
(`peek(state)`/`advance()`), which is exactly the "lexer thread narrowed by parser
state" that `exhaust_lexer`/`resume_parse` need.

**So the refactor is:** extract the two stacks + the feed loop into a private
`ParserStack` with one `feed_token` method, and have `run`, `run_recovering`, *and*
the interactive parser all call it. One definition of "feed a token through the
machine," three callers — the interactive API becomes a *view onto the existing
engine*, which is what keeps it correct-by-construction and portable.

```rust
// private — NOT public surface (keeps raw stacks encapsulated; escalate-clean)
struct ParserStack {
    state_stack: Vec<usize>,
    value_stack: Vec<NodeValue>,
}

impl ParserStack {
    /// The reduce+shift loop lifted verbatim from `run`/`run_recovering`.
    /// Returns Some(tree) iff this token drove ACCEPT (is_end at end-state).
    fn feed_token(
        &mut self, table: &ParseTable, builder: &TreeBuilder,
        token: &Token, is_end: bool,
    ) -> Result<Option<ParseTree>, ParseError> { /* the existing match, factored */ }
}
```

`run`/`run_recovering` then shrink to "pull a token from the source, call
`stack.feed_token`, handle the recovery arm" — a net simplification, and the
banks (512/512 LALR compliance + the recovery oracle) are the regression net that
proves the extraction is behavior-preserving.

---

## 3. Proposed public API (v1)

A single owned, mutable handle obtained from `Lark`. `&mut self` for the mutating
ops, an explicit `fork()` for copies. No re-entrant callback driving a borrowed
parser (that's the lifetime nightmare we avoid — see §5).

```rust
// src/parsers/interactive.rs  → re-exported from lib.rs

/// A driveable LALR parser: feed tokens, inspect what's accepted, fork, resume.
/// Mirrors Python Lark's `InteractiveParser` (issue #168), so its behavior is
/// oracle-checkable. Obtained from [`Lark::parse_interactive`].
pub struct InteractiveParser<'a> { /* parser: &'a, stack, source, result — all private */ }

impl Lark {
    /// Start an interactive parse over `text`. Nothing is consumed until you
    /// `feed`/`exhaust_lexer`/`resume`. LALR-only (same as recovery).
    pub fn parse_interactive(&self, text: &str) -> Result<InteractiveParser<'_>, LarkError>;
    pub fn parse_interactive_with_start(&self, text: &str, start: &str) -> Result<InteractiveParser<'_>, LarkError>;
}

impl<'a> InteractiveParser<'a> {
    /// Feed one already-built token. At `$END` in the end state, returns the tree.
    /// On a no-action token, errors with the same `UnexpectedToken` a normal parse
    /// would (its `expected` set is exactly `accepts()`).
    pub fn feed_token(&mut self, token: Token) -> Result<Option<ParseTree>, ParseError>;

    /// Build + feed a token by terminal *name* — the form `accepts()` returns and
    /// the oracle speaks. Resolves the name against the grammar; errors if unknown.
    /// This is the ergonomic + oracle-aligned entry point (Python feeds `Token(type, value)`).
    pub fn feed(&mut self, terminal: &str, value: &str) -> Result<Option<ParseTree>, ParseError>;

    /// Terminal names that would advance the parser from here — sorted, deterministic.
    /// The primary oracle comparand. Updated after every feed.
    pub fn accepts(&self) -> Vec<String>;

    /// The current state's action row as a typed map (name → Shift|Reduce|Accept).
    /// Python's `choices()`.
    pub fn choices(&self) -> BTreeMap<String, ActionKind>;

    /// Feed a synthetic `$END`, finishing the parse. Python's `feed_eof`.
    pub fn feed_eof(&mut self) -> Result<Option<ParseTree>, ParseError>;

    /// Pull + feed the rest of the lexer (no `$END`); return the tokens consumed.
    /// Python's `exhaust_lexer`.
    pub fn exhaust_lexer(&mut self) -> Result<Vec<Token>, ParseError>;

    /// Resume fully-automated parsing from here to completion. Python's `resume_parse`
    /// (the recovery resume). Consumes the handle — you wanted the tree, not the cursor.
    pub fn resume(self) -> Result<ParseTree, ParseError>;

    /// An independent cursor: its feeds don't affect this one, or vice-versa.
    /// Python's `copy()`. Cheap (see §5 — values aren't deep-copied for inspection).
    pub fn fork(&self) -> InteractiveParser<'a>;

    /// Debug rendering of `choices()` + stack depth. Python's `pretty()`.
    pub fn pretty(&self) -> String;
}

/// What `choices()` reports for a symbol in the current state.
pub enum ActionKind { Shift, Reduce, Accept }
```

### Why these specific choices

- **`feed(name, value)` as the headline ergonomic + oracle method.** `accepts()`
  hands back terminal *names*; the natural next move is to feed one of those names.
  It also matches how the oracle script is written (`feed:NAME:value`), so the test
  harness and a real user drive the parser the *same* way. `feed_token(Token)` stays
  for callers who already hold a `Token`.
- **`accepts() -> Vec<String>` sorted, not a `HashSet`.** Determinism is the
  point — a sorted vec is directly comparable to the oracle's `sorted(accepts())`
  with no set-ordering noise.
- **Owned-mutable + explicit `fork()`, not Python's two classes.** Python ships
  both `InteractiveParser` (mutable) and `ImmutableInteractiveParser` (ops return
  new). In Rust the idiom is one mutable type plus `Clone`/`fork`; the "immutable"
  workflow is just `let b = a.fork(); b.feed(...)`. We don't port
  `ImmutableInteractiveParser` as a second type — that's spelling, not capability,
  so the oracle is unaffected.
- **Raw `state_stack`/`value_stack` stay private.** The other agent's stop-condition
  ("if the design exposes raw stacks publicly, escalate") is honored by
  construction: callers see `accepts`/`choices`/`pretty`, never the Vecs. Keeps it
  portable (PyO3/WASM/C) and lets us change the internal representation later.

---

## 4. The oracle-testing story (the primary deliverable)

This is the part the design is optimized for. Because the surface is 1:1 with
Python, a test case is a **script of operations** run against *both* engines, with
a comparison after every step.

### Generator (`tools/generate_oracles.py::generate_interactive`)

```python
# cases.json: list of { grammar, start, input, script }
# script ops: "accepts" | "feed:<TYPE>:<value>" | "feed_eof" | "exhaust"
ip = parser.parse_interactive(case["input"])
trace = []
for op in case["script"]:
    if op == "accepts":
        trace.append({"op": op, "accepts": sorted(ip.accepts())})
    elif op.startswith("feed:"):
        _, typ, val = op.split(":", 2)
        try:
            ip.feed_token(Token(typ, val))
            trace.append({"op": op, "ok": True, "accepts": sorted(ip.accepts())})
        except UnexpectedToken as e:
            trace.append({"op": op, "ok": False, "expected": sorted(e.accepts)})
    elif op == "feed_eof":
        try:
            tree = ip.feed_eof()
            trace.append({"op": op, "ok": True, "tree": tree_to_json(tree)})
        except UnexpectedToken as e:
            trace.append({"op": op, "ok": False, "expected": sorted(e.accepts)})
    elif op == "exhaust":
        toks = ip.exhaust_lexer()
        trace.append({"op": op, "tokens": [(t.type, str(t)) for t in toks]})
# committed: fixtures/oracles/interactive/cases.json
```

### Replay (`tests/test_interactive.rs`)

For each case, build the Rust `InteractiveParser`, run the same script, and assert
**at each step**:
- `accepts` → the sorted `Vec<String>` equals the oracle's set;
- a `feed` → same ok/err *and*, on err, the same `expected` set (which is just
  `accepts()` again — the `UnexpectedToken.expected` already carries it);
- `feed_eof`/end → `tree_matches_oracle` against the recorded tree (reuses the
  existing comparator);
- `exhaust` → same token (type, value) list.

That gives a **step-granular differential** — far stronger than a single
end-tree compare, and the `accepts()`-after-every-feed trace makes any divergence
in the state machine surface immediately and name a concrete terminal.

### Three free relative oracles (no Python needed)

1. **Resume == normal parse.** For any valid input,
   `parse_interactive(text).resume()?` must equal `parse(text)?`. Re-grounds the
   whole interactive path against our own already-oracle'd parser.
2. **Exhaust+eof == normal parse.** `exhaust_lexer()` then `feed_eof()` on valid
   input == `parse(text)`.
3. **`accepts()` honesty (property).** Every terminal in `accepts()` must
   `feed` without an `UnexpectedToken`; every terminal *not* in it must error.
   This is exactly how Python *computes* `accepts()`, so it's a tautology there —
   but in Rust it's an independent property test that catches a buggy `accepts()`.

Net: v1's behavior is almost entirely covered by Tier-A (Python differential) +
relative oracles. There is **no curated-golden residue** for #168 — which is
precisely why it's the most autonomous-ready of the three recovery features.

---

## 5. Usability notes & the decisions that need you

### Ergonomics that fall out well
- **Feed-by-name** (`accepts()` → pick one → `feed(name, val)`) is a clean loop.
- **`fork()` is cheap** because inspection doesn't need values: `accepts()` clones
  only the `Vec<usize>` state stack and runs a *value-free* feed (REDUCE/SHIFT/GOTO
  depend only on states and rule sizes, never on the tree values). This is actually
  *better* than Python, which copies with `deepcopy_values=False` and still carries
  the value list. Worth a code comment + a perf note.
- **Errors are already right.** A no-action feed returns the same `UnexpectedToken`
  with the `expected` set — no new error type.

### The genuine design forks (escalate-tier; name these in the ADR per §4)

1. **`on_error` integration — v1 or defer?** Two ways a caller can use this:
   - (a) **Direct driving** via `parse_interactive()` — the loop is the caller's.
     Clean, no lifetime trouble. *Recommend as the whole of v1.*
   - (b) **Inside the existing `on_error` callback**, à la Python (handler gets the
     interactive parser). In Rust this means handing `&mut InteractiveParser` into a
     `FnMut` while the parser owns the stacks it's mutating — borrow-checker-hostile,
     and it changes the shipped `on_error` signature. *Recommend deferring (b)* — keep
     the bool `on_error` as-is, ship (a). Most recovery use cases (including
     `match_examples`) only need (a).
   → **Decision:** ship `parse_interactive()` only in v1; revisit handler-integration
     after it's proven. (This is an *API ergonomics* axis — §4 judgment-only, name it.)

2. **Does v1 expose token *insertion*?** Yes — but note it's *caller-directed*, not
   automatic: `feed(name, value)` already lets a caller feed a token the lexer never
   produced. That's exactly Python's `feed_token`, fully oracle-able, and it's what
   makes the automatic #164 redundant. There is no separate "insertion feature" to
   gate — it's just `feed`. (Confirms the #211 hinge: match Python ⇒ insertion is in,
   via `feed`, and #164 stays deferred.)

3. **Lexer ownership / lifetime.** `InteractiveParser<'a>` borrows the `Lark`
   (`&'a`). `exhaust_lexer`/`resume` need a live `TokenSource` over the input, so the
   handle owns a `Box<dyn TokenSource + 'a>` built at `parse_interactive` time. Open
   sub-question: do we support `parse_interactive` under the *contextual* lexer in v1
   (the source already narrows by state, so yes in principle) or restrict v1 to the
   basic lexer and add contextual in a follow-up? *Recommend: basic lexer first*
   (smaller blast radius), contextual as a fast follow with its own oracle case —
   the contextual `TokenSource` already exists (`ContextualRecovering`).

4. **Portability check (§4 named axis).** The handle holds Vecs + a boxed source;
   `fork()`/`Clone` of the value stack must use the existing worklist `Clone` (#151)
   so a deep stack doesn't overflow WASM's ~1 MB native stack. PyO3 maps the handle
   to a Python class 1:1 (bonus: closes a real Python-parity gap for our PyO3 users);
   the C API uses an opaque-pointer handle. Confirm the binding jobs cover it.

---

## 6. Recommendation & phasing

**v1 (this issue):**
- Extract `ParserStack::feed_token` (refactor; banks are the net).
- Ship `Lark::parse_interactive` + the `InteractiveParser` surface in §3, **basic
  lexer**, exactly mirroring Python's ops (no extras).
- Land the `generate_interactive` oracle + `test_interactive.rs` differential **and**
  the three relative-oracle property tests (§4). This is the falsifiable done-when.

**Fast follow (separate issues):**
- Contextual-lexer `parse_interactive` (own oracle case).
- `on_error`-callback integration (fork 1b) *only if* a consumer needs it.

**Done-when (proposed for #168):**
> `Lark::parse_interactive` exposes feed/`accepts`/`choices`/`feed_eof`/
> `exhaust_lexer`/`resume`/`fork` over the basic lexer, mirroring Python's
> `InteractiveParser`; a step-granular differential oracle
> (`tools/generate_oracles.py::generate_interactive` + `tests/test_interactive.rs`)
> pins `accepts()` traces + resulting trees against Python across a curated script
> bank, and three relative-oracle property tests (resume==parse, exhaust+eof==parse,
> `accepts()` honesty) hold. No operation is exposed that Python lacks.

This is fully Tier-A/relative-oracle grounded — **`good-autonomous` once the
architect ratifies the surface** (the API *shape* is the only judgment call; its
*behavior* is entirely oracle-checked). Merge tier: **escalate** (new public API).

### Escalate-tier decisions for the architect to confirm
1. The v1 surface in §3 (names + the one-mutable-type-plus-`fork` shape).
2. `parse_interactive`-only in v1; defer `on_error`-callback integration (fork 1).
3. Basic lexer first; contextual as a fast follow (fork 3).
4. "Exactly Python's ops, no extras" as the standing scope rule for this API.
