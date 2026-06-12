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

One review, one CI run per task; post-PR pushes should only be fixes for
genuinely CI-environment-specific failures. `lark-rs/scripts/check.sh` (the
full gate, mirroring CI's `fmt` + `test` jobs exactly) is for reproducing a
red CI locally — not a routine pre-push step.
