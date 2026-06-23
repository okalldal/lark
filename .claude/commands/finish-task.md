---
description: End-of-task loop — code review, fast gate, PR, CI callback
---

Run the lark-rs end-of-task loop. Do **NOT** run the full CI locally before
pushing — that runs everything twice (once here, once in GitHub Actions).

1. **Review first.** Run `/code-review` on the branch diff and apply the
   findings now, *before* creating the PR. The review runs in a fresh subagent
   either way (it sees only the diff, not this session's reasoning), and fixing
   findings pre-PR means CI runs once on the final diff instead of twice.
   Note what the review flagged and how it was addressed for the PR description.

   **Review→push→PR→return is ATOMIC — never strand the work (#309).** Run the
   review **inline / foreground in this turn**: do **NOT** spawn a *background*
   sub-agent for it and then idle — the harness ends the turn while you wait on
   the notification, leaving validated-but-unpushed work stranded (this happened
   3× in sprint #284). If you launch a review sub-agent, **await it synchronously
   (foreground)** and **never end the turn while your own sub-agents are still
   running**. Once the findings are addressed, carry straight through to the
   `push → open-PR` (steps 2–3) in the **same** turn — opening the PR is the last
   action of the turn, not deferred to a follow-up.

   **Differential-audit checkpoint (make it a conscious, recorded decision).**
   Ask: does this change touch a behavior whose *full* input space the standing
   banks do **not** exhaustively cover (nullable / EBNF-expansion edges, ambiguity
   dedup, recovery resync, lexer tie-breaks, …)? The banks are a *regression* net,
   not a *completeness* net (cf. #101: a CYK fix passed all four banks yet
   over-rejected `start: A (B*)~2`, which Python Lark accepts). If yes, **run a
   targeted differential audit against Python Lark** over a handful of adversarial
   inputs in that space — not just the committed bank — and pin any new case found.
   If no oracle makes the audit impractical, say so explicitly and treat
   banks-green as *necessary but not sufficient*. Record the outcome (audited / not
   applicable / impractical) in the PR body so the reviewer can check it.

2. **Fast gate** (the Pareto cut — fmt + `cargo test --all` catches nearly
   every red):
   ```bash
   lark-rs/scripts/check-fast.sh
   ```
   Run more than the fast gate only if:
   - You touched `tools/` generators or `tests/fixtures/oracles/` → also run
     the oracle-freshness regen (`lark-rs/scripts/check.sh` step 3) so a
     stale-oracle red doesn't cost a CI round trip.
   - You touched `lark-rs/python/` or `lark-rs/wasm/` → also run that crate's
     own tests (`maturin develop && pytest` / `npm test`).

3. **Push and create the PR right away** — the `pull_request` run IS the full
   CI (fancy-oracle differential, scaling gates, python.lark LALR gate, oracle
   freshness, python/wasm binding jobs). Branch pushes alone do not trigger CI;
   the PR does. Include the review summary from step 1 in the PR description.

4. **Subscribe to the PR's activity** (`subscribe_pr_activity`) and fix any red
   from the CI callback.

5. **Close out against the Definition of Done** (`lark-rs/docs/PRINCIPLES.md`
   §6). Before considering the task finished:
   - **File follow-ups, don't bury them.** Any bug or out-of-scope work found
     mid-task is filed as an issue in the §7 contract shape (Done-when / Priority
     / Files / Notes), never silently fixed or dropped — this is how #159, #101,
     #64, #59 came to exist. Label them per `lark-rs/docs/LABELS.md`.
     When a follow-up is an unresolved fork or public API/product decision, file
     it with the decision memo skeleton (see `/roadmap`) and label it
     `needs-decision`, not just a generic issue.
     **Process debt counts too (required, checkable — not implicit).** If the task
     surfaced a *retro-flagged kaizen item* — a kit/process fix, a "KIT BUG," a
     "file as follow-up" note about a stale instruction, a misbehaving tool, or
     missing know-how — file it as a `kaizen`-labelled issue (`lark-rs/docs/LABELS.md`)
     **before** reporting the task done, and link it in the PR close-out; the task is
     not done until each such note is filed or explicitly marked already-tracked. This
     mirrors the sprint/kaizen §9 close-out: the close-out step itself must obey §7's
     "never silently drop" (the lapse #284 made and #315 fixed).
   - **Write an ADR if you deviated from a §3 default**, or made an
     architecture / public-API call a future reader would have to reverse-engineer
     (`lark-rs/docs/decisions/`, copy `TEMPLATE.md`). Commit it in *this* PR
     and link it from the body. Skip it for routine, fully-gated work — the test
     is the record there. **A staged ADR is always `Status: Proposed` — never
     self-ratify.** An agent does not have the authority to accept its own
     decision; only the **architect** ratifies, by flipping it to `Accepted` when
     they merge the PR. Author it `Status: Proposed (pending architect
     ratification)` (the canonical `TEMPLATE.md` phrasing) and say so in the PR
     body; a self-authored `Status: Accepted` is a DoD failure.
     **Renumber-on-rebase (#207):** the ADR number is the next free integer *at
     authoring time*, but `master` moves — before rebasing onto `master`, re-check
     the highest ADR number there and renumber your `Proposed` ADR to the next free
     slot (a `git grep ADR-NNNN` reference sweep covers the file, the
     `decisions/README.md` index row, and any code/`CLAUDE.md` citation). Doing it
     pre-rebase keeps the parallel-branch collision routine, not a conflict surprise.
   - **Point the PR at its issue:** the body must say `Closes #N`, and the
     done-when must actually be met.
   - **One PR, one concern** (§9): if your work touched both code and a
     governance/policy doc (this constitution, ADRs, command behavior, `LABELS.md`),
     split them — agents don't change their own authority while shipping code.
   - **Merge tier** (§6 / ADR-0016, **Accepted**): classify as `auto` (bugfix-with-oracle,
     xfail burndown, perf-fix-behind-a-gate, *trivial* docs, refactor with banks green)
     or `escalate` (new public API, new grammar semantics, architecture, or **any
     governance/policy doc**). Run `/review-pr` for the verdict. With ADR-0016 accepted,
     `/review-pr` **merges an `auto`-tier PR directly once the DoD is met**; an
     `escalate`-tier PR (and anything `needs-decision`) is left for the **architect** to
     merge — never self-merge those. Governance/policy PRs are always `escalate`.
     (Inside a `/start-sprint` run the rules differ — review is verdict-only and nothing
     merges to `master` outside the omnibus PR; see `.claude/commands/start-sprint.md`.)

One review, one CI run per task; post-PR pushes should only be fixes for
genuinely CI-environment-specific failures. `lark-rs/scripts/check.sh` (the
full gate, mirroring CI's `fmt` + `test` jobs exactly) is for reproducing a
red CI locally — not a routine pre-push step.
