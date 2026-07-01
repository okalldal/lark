# Notes on Verifiability

*An essay on what rewriting Lark in Rust taught us about building software you
can trust — and how far those lessons travel.*

Lark is being rewritten from scratch in Rust. A rewrite of a mature, behaviour-rich
parser sounds like the kind of project that should be a long march of subtle bugs:
thousands of grammar features, three parsing algorithms, a contextual lexer, EBNF
expansion, error recovery — each with corners that only reveal themselves on the
one input nobody tested. In practice it has been remarkably calm. That calm comes
from a single idea, and the idea turns out to be far more general than parsing. This
essay is an attempt to say what the idea is, why it works, and where it stops
working.

## Parsers are hard to write but easy to check

Start with the observation that makes parsing a fortunate problem. Writing a correct
parser is genuinely difficult. *Checking* whether a given parser is correct is, by
comparison, trivial: feed it an input, and there is exactly one right answer — the
parse tree. You don't have to understand *how* the parser arrived at the tree to
know whether the tree is right. You just compare it against a reference.

This gap — hard to produce, easy to verify — is the same asymmetry that sits under
a great deal of modern computing, from the P-versus-NP question to the way
reinforcement learning systems improve. A Sudoku is hard to solve and instant to
check. A chess move is hard to find and, with an engine, easy to score. Whenever a
problem has this shape, you can make progress you could never make by careful
authorship alone, because you can *try things* and let the check tell you whether
you were right.

Lark has a particularly good version of this asymmetry, because a reference answer
already exists: the original Python implementation. For any grammar and any input,
Python Lark already computes the one right tree. So the Rust rewrite never asks a
human "is this output correct?" It asks a program "does this match Python Lark?" —
and answers itself. We call that reference the **oracle**, and the decision to lean
on it completely is the first and most important one we wrote down
([ADR-0001: Python Lark is the oracle][adr1]).

> "Traditional computers automate what you can specify in code. AI/LLMs automate
> what you can verify." — Andrej Karpathy

## It isn't purity — it's the oracle

It's tempting to say the thing that makes a parser verifiable is that it's a *pure
function*: same input, same output, no hidden state. That's true, and it helps — a
pure function can be recorded and replayed, sampled cheaply, and trusted to behave
the same way tomorrow. But purity is not really the load-bearing property. A
database, an operating system, a payroll run are all "pure functions" too, if you
are willing to write the entire state of the world into the input. The reason we
don't *think* of them that way points at what actually matters.

What actually matters is the quality of the oracle, and that decomposes into a few
plain questions:

- **Does a trusted reference even exist?** For the rewrite, yes — a mature
  implementation we already believe.
- **Is it cheap to consult?** Python Lark runs in milliseconds, for free, with no
  side effects. We can ask it about a million inputs without consequence.
- **Can it answer about *any* input, or only some?**
- **Is its answer itself trustworthy,** or does matching it mean faithfully
  reproducing its mistakes?
- **Can you compare answers exactly?** A parse tree has a clean, decidable notion
  of equality. "These two trees match" is a fact, not a judgement call.
- **Can you sample inputs that represent what real users will do?**

A problem is "parser-shaped" — safely, almost boringly verifiable — when all of
these line up at once. Purity is just one of the things that makes the cheap,
repeatable sampling possible. The oracle is the thing.

## From checking code to trusting automation

Once you have an oracle this good, something changes about *who* has to be in the
loop. A reviewer no longer has to read every line and vouch for its correctness,
because correctness has been converted into a question the machine settles by
itself. That turns out to be the key that unlocks automating the development work,
not just the testing: a change is allowed to land on the strength of the oracle
agreeing, without a human adjudicating taste on every commit.

We ended up stating this as a general principle for how the project governs itself:

> The boundary of safe autonomy is the boundary of what we have made falsifiable.

Anything an automated contributor can ground against the oracle — a feature, a bug
fix, a refactor — it may decide and self-check. Anything it *cannot* ground — a
genuine product trade-off, a question of taste, a behaviour the oracle has no
opinion about — it must escalate to a human. The whole [development
constitution][principles] is, in the end, just machinery for locating that line and
routing work across it. Verifiability isn't only a testing tactic; it's the thing
that decides how much can be trusted to run unattended.

## How far does this travel? The case of legacy migration

Here is the question worth dwelling on, because it is where most software actually
lives. A great deal of valuable engineering is not greenfield invention but
*migration*: lifting a legacy business system onto a modern stack while preserving
what it does. How much of the parser story carries over?

The good news is the best part carries over for free. In a migration, **the legacy
system is its own oracle.** You don't have to invent a reference for correct
behaviour; the thing you're replacing already embodies it. This is an old idea with
a name — *characterization testing*: before you touch a legacy system, you pin down
what it currently does by capturing its inputs and outputs, exactly as we capture
Python Lark's trees. The industrial-scale version is the **parallel run** (or
"shadow" deployment): route real traffic to both the old and new systems at once,
compare their outputs, and trust the new one only where it agrees. Real production
traffic even solves, for free, the awkward "do we have representative inputs?"
problem — your users are generating the representative distribution every day.

The bad news is the things that made parsing *easy* are precisely the things a
business system tends not to have, and the gaps are instructive:

- **A parser's input is a single string; a business system's input is a
  *trajectory*.** One flat string is trivial to generate and to enumerate. A real
  system's behaviour depends on a long sequence of events threaded through
  accumulated state, and the bug you fear appears only after one specific sequence.
  Verifying trajectories is exponentially harder than verifying points — and you
  cannot simply throw random API calls at a banking system, because almost all of
  them are rejected before they reach the logic you wanted to test.

- **A parser is deterministic by nature; a real system leaks hidden inputs.** The
  clock, randomness, concurrency, a third-party service, the user's locale — each is
  an input that isn't written in the signature, and each one breaks the clean
  "same input, same output" that replay depends on. Before you can shadow-test such
  a system at all, you usually have to *manufacture* determinism: isolate the clock,
  record external calls, pin down concurrency. That work is often the bulk of the
  migration, not a preliminary to it.

- **A parse tree has exact equality; a business output usually doesn't.** Two runs
  differ in timestamps, in generated IDs, in row ordering, in how a number is
  formatted — none of which *matter*. So you can't ask "are the outputs identical?";
  you must define which differences are essential and which are incidental. And that
  definition is a judgement, not a test.

That last point is the deep one, and it deserves its own section.

## The hardest part is deciding which differences matter

When the Rust rewrite disagrees with Python Lark, the naïve instinct is "the rewrite
is wrong, make it match." But sometimes Python Lark's behaviour is an accident of
how it happens to be built rather than a promise it means to keep — and slavishly
reproducing an accident is its own kind of bug. We had to write down a rule for
this ([ADR-0017: oracle fidelity is for *intended* behaviour, not implementation
artifacts][adr17]): match the reference's intended contract, but feel free to
diverge from mere leakage of how it was built — deliberately, and on the record.

Migration runs straight into the same wall, only larger and with money attached. A
legacy system's behaviour is a mix of contract and accident: rules the business
depends on, bugs the business has *come* to depend on, and bugs nobody depends on
at all. The oracle — the old system — can tell you faithfully *what* it does. It can
never tell you *whether you should keep doing it.* That adjudication is irreducibly
human. The oracle is the floor and the ceiling of what can be automated; deciding
which of its behaviours are sacred is the part that stays a conversation.

## A ladder, not a switch

Putting it together, "verifiable" is not a yes-or-no property. It's a ladder, and
the interesting engineering is figuring out how high up the ladder you can pull each
piece of behaviour:

1. **A full oracle** — an independent reference that answers any input exactly. The
   parser's luxury; rare elsewhere.
2. **A partial oracle** — a reference that answers *some* inputs cheaply, and is
   silent on the rest.
3. **Relative oracles** — when you have no ground truth, check *relations* that must
   hold anyway: that round-tripping a value through the system returns it unchanged,
   that an irrelevant change to the input leaves the output untouched, that two paths
   to the same result agree. (This is sometimes called *metamorphic testing*.)
4. **Generated inputs against a reference** — rather than hoping a hand-written test
   set is representative, generate inputs by the thousand and let the oracle judge
   each one. We do exactly this, turning the static reference into an active
   fuzzer that hunts for the inputs nobody thought to write down
   ([ADR-0012][adr12]).
5. **Properties and invariants** — weaker still: not "what is the right answer" but
   "no answer may ever look like *this*."
6. **Human judgement** — the residue at the bottom: taste, product direction, which
   inherited bugs are now features. No test reaches here, and pretending otherwise is
   how trouble starts.

A parser sits comfortably on the top rung. A legacy business system is a *mixture*:
its data-transformation core can often be lifted near the top with characterization
tests and a parallel run, while its trajectory-dependent, time-dependent, and
"do-we-keep-this-bug" parts sit near the bottom and need real human judgement plus
old-fashioned caution — gradual rollout, the ability to roll back. The skill is not
declaring the whole system verifiable or not; it's sorting its behaviours onto the
right rungs and being honest about which ones never made it past the bottom.

## A closing thought on where knowledge comes from

There's a final idea hiding in all this, and it's epistemological. There are two
ways a system — a person, an organization, a learning algorithm — can come to know
how to do something. It can *imitate*: study a corpus of existing solutions and
reproduce them, in which case it can never really exceed its teachers. Or it can
*search*: try things and keep what a reliable check confirms, in which case it can
discover solutions present in no corpus at all. The difference between learning from
recorded games and learning by playing millions of your own is the difference
between these two, and it is why self-play can blow past human play while imitation
cannot.

A good oracle is what makes the second mode possible, because it is a reliable check
you can consult as often as you like. That is the quiet reason the Lark rewrite
feels less like transcription and more like exploration: with the oracle in hand, we
can *try* an implementation and find out, immediately and without a human, whether
it was right. The honest caveat is that our oracle is itself an imitation of Python
Lark, so this freedom has a horizon — we can discover, freely and safely, *any way
of reproducing Python's behaviour*; we cannot discover what parsing *ought* to do
beyond Python without building a new oracle or asking a person. Knowledge can be
gained, by trial against a check, exactly as far as the check reaches. Past that, it
has to be gathered, or decided. Knowing which situation you are in — and being
honest about where the check runs out — is most of the craft.

---

*The decisions referenced above live in the rewrite's [decision log][decisions];
the principles that tie them together are in the project's [development
constitution][principles]. They are written for contributors to the Rust rewrite,
but the ideas are general, which is the whole point of this essay.*

[adr1]: https://github.com/lark-parser/lark/blob/master/lark-rs/docs/decisions/0001-python-lark-is-the-oracle.md
[adr12]: https://github.com/lark-parser/lark/blob/master/lark-rs/docs/decisions/0012-differential-fuzzer-active-oracle.md
[adr17]: https://github.com/lark-parser/lark/blob/master/lark-rs/docs/decisions/0017-oracle-fidelity-is-for-intended-behavior.md
[principles]: https://github.com/lark-parser/lark/blob/master/lark-rs/docs/PRINCIPLES.md
[decisions]: https://github.com/lark-parser/lark/tree/master/lark-rs/docs/decisions
