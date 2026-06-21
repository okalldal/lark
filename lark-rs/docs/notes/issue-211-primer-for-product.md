# Primer — what "error recovery" is, and why #211 needs *you*

**Audience:** the product owner, not a parser engineer. No jargon without a plain
explanation. This is the "why are we even talking about this" memo that sits
*under* the two technical memos in this folder.

---

## 1. What a parser does (30 seconds)

A **parser** reads text written by a human — source code, a config file, a JSON
blob — and turns it into a **tree**: a structured object the rest of a program can
actually work with. "Lark" is a parser toolkit; "lark-rs" is our Rust rewrite of
it.

> Think of the parser as the thing that turns the *string* `"1 + 2"` into the
> *idea* `add(1, 2)`. Everything downstream (an interpreter, a compiler, an
> editor's autocomplete) works on the idea, not the raw text.

When the text is well-formed, this is easy. The interesting question is: **what
happens when the text is broken?**

---

## 2. What "error recovery" is, and why an editor lives or dies on it

By default, a parser hits the first mistake and **stops**: "Syntax error, line
12." That's perfectly fine for a compiler you run once on finished code.

It is a disaster for a **code editor** (VS Code) or a **language server** (the
"LSP" you keep seeing — the background brain that powers autocomplete, red
squiggles, go-to-definition, hover docs). Why? Because **code in an editor is
almost always broken** — you're in the *middle of typing it*. The instant you
type `foo(` and haven't closed the paren yet, the file is technically invalid.

If the parser gave up at the first error, every editor smart-feature would switch
off the moment you have a single typo or half-typed line. Useless.

**Error recovery** is the parser's ability to say: *"I hit something broken here.
I'll make a note of it, patch over it, and keep going"* — so it still produces a
mostly-complete tree despite the mistakes. **That partial-tree-plus-list-of-errors
is the raw material every editor feature is built on.** Red squiggle = "here's an
error from the list." Autocomplete = "I still understood the tree around your
cursor even though line 12 is broken."

So: recovery is not a nice-to-have. It is *the* feature that makes a parser usable
for editor/LSP tooling. That's the whole point of the epic.

---

## 3. What we already have, and what the epic wants to add

We already shipped the **simplest possible** recovery: when the parser hits a bad
token, it **deletes that one token and carries on.** ("single-token deletion.")
Crude, but real, and it works.

The epic (**#209**) is about the **next tier** — the stuff real editors actually
want. There are three pieces:

| # | In plain terms | The editor wants it so it can… |
|---|---|---|
| **#164** | **Smarter patching.** Instead of only *deleting* the bad token, also *insert* a missing one (you forgot a `)`) or *skip ahead* to a safe restart point (the next `;`). | …recover gracefully from the messy ways people actually mistype, not just delete-and-pray. |
| **#165** | **Put the errors *inside* the tree.** Today errors come back in a separate list *next to* the tree. This puts a marker *in* the tree at the broken spot. | …highlight the *exact* broken sub-part on screen (this is how tree-sitter, the thing GitHub uses for highlighting, works). |
| **#168** | **A "driveable" parser.** Hand the editor the steering wheel: let it ask the parser "what would be valid right here?" and direct the recovery itself. | …implement its *own* recovery cleverness, tuned to its language, instead of being stuck with ours. |

That's the product ambition: make lark-rs good enough to sit under editor/LSP
tooling. The direction is **already approved.** Nothing about *whether we want
this* is in dispute.

---

## 4. The fuss: why these three can't just be built

Here is the thing that makes this project special, and also the thing that's
blocking us.

**This project's superpower is that it checks its own work automatically.** For
any input, we ask the original Python Lark — the trusted reference, which we call
the **"oracle"** — *"what tree do you produce?"* and we demand our Rust version
produce the **identical** tree. That automatic check is *why* AI agents can do
most of the work here unsupervised: an agent never has to **guess** whether it got
something right, it can **check**. ("The boundary of safe autonomy is the boundary
of what we've made checkable" — that's the principle you already get.)

The simple deletion recovery we shipped was checkable this way **for free**,
because Python Lark happens to do the *exact same* delete-and-continue thing. We
just compared, token for token. Done.

**The three new features don't have a Python counterpart to check against.** That
is precisely, concretely, why they're "not falsifiable right now":

- **#164 (insert a token / skip to a resync point):** Python Lark *doesn't do
  this automatically.* There is no reference answer. If our parser, on a missing
  `)`, chooses to insert it *here* versus *there* — who says which is correct?
  Nobody. It's a judgment call, not a checkable fact.
- **#165 (errors inside the tree):** Python *deliberately keeps errors outside the
  tree.* The moment we put a marker *inside*, the tree shape is something Python
  never produces — so there's nothing to compare it to.
- **#168 (driveable parser):** Python has *some* of this, so we can check the
  overlapping parts — but the rest is new controls Python never had, and again,
  no reference.

So **"not falsifiable" means: we can't auto-check whether the new behavior is
correct, because there's no authority that defines what correct even is.** And the
project's rule forbids letting an agent build something it can't check itself — it
must *either* find a way to make it checkable, *or* a human (you) decides.

That's the whole fuss. Not "is this hard to code" — it's "**how do we know it's
right, if Python can't tell us?**"

---

## 5. The actual trade-offs (what the two memos are arguing about)

The debate is entirely about **how to make these checkable *enough* to let agents
build them safely** — versus parking them as human-judgment work. There are three
grades of "check," strongest to weakest:

1. **Compare to Python (gold standard).** Not available for most of this — that's
   the whole problem.

2. **Clever *indirect* checks.** This is the useful insight in my memo:
   - **#165 trick:** even though "errors inside the tree" is a new shape, if you
     *erase the error markers*, what's left must be **exactly** the tree Python
     produces. So we still lean on Python — indirectly.
     > *Analogy:* you can't check my annotated map against the original, but if I
     > peel off all my sticky notes, the map underneath must match the original
     > **exactly**. If it doesn't, I corrupted the map — caught automatically.
   - **#168 trick:** drive *both* Python's parser and ours through the same
     sequence of button-presses, and check they agree on every button Python also
     has.
   - **#164 trick (weaker):** we can't check the *exact* patch, but we *can* check
     **sensible properties**: it never loops forever, it never produces a *worse*
     result than plain deletion, and whatever it produces is itself valid.

3. **Hand-write the expected answers once, have a human eyeball them, and freeze
   them.** The weakest net — it's a human *asserting* correctness once, not an
   independent automatic check. Fine for the small leftover bits the cleverer
   checks can't cover (e.g. *exactly where* an error marker goes).

**The disagreement between the two memos, in one line each:**
- **The other agent:** treat all three as "a human must nail down the design
  first, then build." Safest, slowest, most human-in-the-loop.
- **My memo:** #165 and #168 are *more* checkable than they look (the indirect
  tricks above), so agents can build them sooner *with* real safety nets; only
  **#164** genuinely lacks a good check and should wait — or be dropped entirely.

---

## 6. The one decision that settles most of it

Most of the back-and-forth collapses to a single product-flavored choice:

> **Should the "driveable parser" (#168) let the editor insert missing tokens
> itself?**

- **If YES:** editors can do their own smart patching → we **don't need** to build
  the risky automatic-patching feature (#164) at all → build #168 first, drop or
  defer #164. *(My lean.)*
- **If NO:** keep #168 small and safe → but then we **still need** #164 for smart
  patching → build #168 later. *(The other agent's lean.)*

Same facts, opposite call. It's a real fork, and it's the kind only you (or your
architect) should make — there's no oracle that decides it.

---

## 7. What's actually being asked of you

You do **not** need to understand parsers. You need to make a few product calls:

1. **Appetite.** Do we want editor/LSP support badly enough to invest here *now*?
   (The issues themselves hint our *current* users are already covered by the
   simple deletion recovery — this tier is for a *future* editor/LSP audience.)
2. **Risk tolerance for the checks.** For the parts with no gold-standard, are you
   OK relying on the **indirect checks + occasional "a human eyeballs it once"** as
   the safety net (lets agents move faster) — or do you want a human to sign off on
   each design *before* any agent builds it (slower, safer)?
3. **The insertion fork** in §6, which decides the build order and whether #164
   lives at all.

Answer those three and the six loose technical sub-questions resolve themselves.
Everything in the other two memos is just the detailed version of these.
