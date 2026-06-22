---
description: Run a structured 10-team adversarial oracle sweep and open a findings-only XFAIL/catalog PR
---

# /bug-hackathon — adversarial oracle sweep

Run a structured bug-bounty style adversarial testing sweep against lark-rs.

The command's job is to produce a **findings-only PR**: a catalog of confirmed, minimized, oracle-backed bugs, plus ignored XFAIL tests that currently fail against lark-rs and can later be unignored when fixed.

Do not fix bugs in the same PR.

---

## Inputs

Optional arguments:

- `--round <name>`: round label, e.g. `h3`, `phase3`, `2026-06-22-h3`.
- `--teams <n>`: number of strike teams. Default: `10`.
- `--base <sha-or-branch>`: explicit baseline. Default: current `HEAD`.
- `--scope <text>`: extra user-specified focus.
- `--exclude <issue-or-id>`: extra ineligible issue/root-cause IDs.
- `--continue-from <catalog>`: prior bounty catalog to dedup against.

If not specified, infer prior catalogs from:

- `lark-rs/docs/BOUNTY_FINDINGS*.md`
- open GitHub issues
- recently merged PRs
- current XFAIL files

---

## Hard rules

1. **Freeze the baseline.**
   Record the exact commit SHA before launching teams.
2. **Python Lark is the oracle.**
   A correctness finding must compare against Python Lark unless the finding is explicitly a relative-oracle, property, performance, or distribution finding.
3. **Findings-only PR.**
   Do not change production behavior in the hackathon PR. Add only:
   - docs/catalog;
   - XFAIL tests;
   - harness/test utilities if needed;
   - optional scripts used to reproduce findings.
4. **No duplicate payouts.**
   A report is fresh only if it has a new root cause. New surfaces of an existing root cause are variants.
5. **Evidence before severity.**
   Full payout needs executable evidence:
   - A-level: executable oracle test;
   - B-level: executable direct API or property test;
   - C-level: source-traced plus empirical notes, provisional only;
   - D-level: hypothesis, not payable.
6. **Minimize.**
   Every bug must be reduced to the smallest readable grammar/input/options tuple that preserves the divergence.
7. **Name the expected fix contract.**
   Each finding must say whether the fix should:
   - support and match Python;
   - reject like Python;
   - reject with a documented lark-rs divergence;
   - preserve an intentional divergence via ADR.
8. **Do not overclaim clean buckets.**
   A clean team result is useful negative evidence, not proof of correctness.

---

## Phase 1 — Preflight

1. Ensure the tree is clean.
2. Identify baseline SHA:
   ```bash
   git rev-parse HEAD
   ```
3. Read the project ground truth:
   - `lark-rs/CLAUDE.md`
   - `lark-rs/docs/PRINCIPLES.md`
   - `lark-rs/docs/STATUS.md`
   - `lark-rs/ARCHITECTURE.md`
   - existing `docs/BOUNTY_FINDINGS*.md`
4. Inspect open issues and recent PRs for known root causes.
5. Build the ineligible set:
   - all prior RC/N/V identifiers;
   - open issues that already describe the same bug;
   - merged PRs not yet on the selected baseline if the user declares them ineligible;
   - documented intentional divergences.

Write a short preflight summary:

```
## Bug hackathon preflight
Baseline: `<sha>`
Round: `<round>`
Teams: `<n>`
Known/ineligible:
- RC...
- N...
- issues...
Allowed scope:
- ...
Out of scope:
- fixes
- duplicates
```

---

## Phase 2 — Generate 10 seed briefs

Generate exactly 10 team briefs unless `--teams` says otherwise.

Each brief must include:

- mission;
- target files/modules;
- likely bug classes;
- concrete seed grammars/options;
- oracle method;
- known duplicates to avoid;
- severity expectations;
- evidence required.

Default team map:

1. Negative grammar conformance
2. Regex width/ranking/token ordering
3. Python `re` dialect and refusal taxonomy
4. Standalone and `include_lark!` compile-run
5. Binding surface matrix
6. Cross-backend validation consistency
7. Tree-shaping algebra fuzzer
8. Transformer/semantic-output parity
9. Wild-bank expansion and hostile real grammars
10. Deterministic performance/resource bounds

Retarget teams if prior rounds exhausted a bucket.

---

## Phase 3 — Launch sub-agents

Launch one sub-agent per team.

Each sub-agent prompt must include:

```
You are Team <n>: <name>.
Baseline: <sha>
Ineligible root causes/issues: <list>
Mission:
...
You must return:
1. confirmed fresh findings;
2. minimized repro grammar/input/options;
3. Python oracle result;
4. lark-rs result;
5. root-cause hypothesis;
6. nearest known issue/root cause and why this is different;
7. evidence level A/B/C/D;
8. suggested severity;
9. exact test/catalog entry;
10. clean-bucket notes if no finds.
Rules:
- Do not fix bugs.
- Do not hand-edit generated oracle artifacts.
- Prefer executable tests.
- If source-only, mark provisional.
- Stop and minimize each promising divergence before expanding.
```

Use the Agent tool available in Claude Code for parallel execution. If the tool cannot run parallel agents, run the briefs sequentially but keep the reports separate.

---

## Phase 4 — Intake and dedup

For every submitted finding:

1. Re-run the repro.
2. Check against Python Lark.
3. Check against known issues and prior catalogs.
4. Dedup by root cause, not by grammar string.
5. Classify:
   - fresh root cause;
   - variant of fresh root cause;
   - variant of known root cause;
   - duplicate/known;
   - invalid finding;
   - intentional documented divergence;
   - harness artifact.

Reject or downgrade any finding whose expected contract conflicts with project policy.

Payable root cause checklist:

- [ ] Python oracle or approved relative oracle exists
- [ ] lark-rs result differs
- [ ] minimal repro included
- [ ] not in prior RC/N/V list
- [ ] expected fix contract stated
- [ ] evidence level A or B for full payout

---

## Phase 5 — Create XFAIL tests

Create a new test file:

```
lark-rs/tests/test_bounty_findings_<round>.rs
```

Rules:

- Mark each failing test with `#[ignore = "XFAIL (...): reason"]`.
- Each test asserts the expected fixed behavior.
- Each test should fail today when run with:
  ```bash
  cargo test --test test_bounty_findings_<round> -- --ignored
  ```
- If a finding is not executable, do not pretend it is. Put it only in docs as provisional, or add a source-level/property test if possible.

Use helper functions for common parser options.

Do not add production fixes.

---

## Phase 6 — Create catalog

Create:

```
lark-rs/docs/BOUNTY_FINDINGS_<ROUND>.md
```

Required structure:

```
# lark-rs bug-bounty findings — <round>
## Target and method
- Baseline SHA:
- Oracle:
- Harness:
- Ineligible set:
- Reproduction command:
## Accounting
- Fresh root causes:
- Variants:
- Known duplicates:
- Provisional/source-only:
- Invalid/rejected reports:
## Severity summary
| ID | Severity | Fresh? | Evidence | Bucket | One-line |
## Findings
### <ID> — <title>
- Severity:
- Evidence:
- Freshness:
- Grammar:
- Input:
- Options:
- Python result:
- lark-rs result:
- Root cause:
- Expected fix contract:
- Nearest known issue/root cause:
- Why distinct:
- Test:
- Affected surfaces:
- Unaffected surfaces:
## Variants
...
## Clean buckets
...
## Harness caveats
...
```

---

## Phase 7 — PR

Create a branch:

```bash
git checkout -b claude/bug-hackathon-<round>
```

Commit only:

- `docs/BOUNTY_FINDINGS_<ROUND>.md`
- `tests/test_bounty_findings_<round>.rs`
- any new harness utilities needed for reproduction

Open PR with title:

```
test(bounty-<round>): <n> fresh oracle divergences as XFAIL tests
```

PR body must include:

```
## Summary
...
## Accounting
- Fresh root causes:
- Variants:
- Known duplicates:
- Provisional:
## Reproduction
`cargo test --test test_bounty_findings_<round> -- --ignored`
## Scope
Findings-only PR. No production behavior changed.
## Review notes
- Expected fix contracts checked.
- Prior RC/N/V dedup performed.
- Source-only findings marked provisional.
- Harness caveats documented.
## Merge tier
Escalate-tier: findings catalog and new XFAIL bank.
```

---

## Phase 8 — Review checklist

Before finalizing, run this self-review:

1. Does every counted fresh root cause have a distinct root cause?
2. Does every full-payout item have A/B evidence?
3. Are variants clearly marked as variants?
4. Are known issues excluded?
5. Are any expected contracts wrong?
6. Are there stale counts in PR title, body, docs, and test header?
7. Does the ignored-test command count match the catalog?
8. Did we avoid production fixes?
9. Did we avoid hand-editing generated artifacts?
10. Are clean-bucket claims modest?

If any answer is no, fix the catalog before opening the PR.

---

## Output

End with:

```
Bug hackathon complete.
PR: <url>
Baseline: <sha>
Fresh root causes: <n>
Variants: <n>
Known duplicates: <n>
Provisional: <n>
Ignored test command: <cmd>
Recommended payout table: ...
```
