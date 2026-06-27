#!/usr/bin/env bash
#
# SessionStart hook — prepares a fresh container (Claude Code on the web) so the
# lark-rs Rust test/lint loop works the moment the session opens, then prints a
# concise status banner.
#
# Design notes:
#   * The image already ships the Rust toolchain + a warmed crate cache (a plain
#     `cargo build --offline` succeeds), so there is nothing to *install* for the
#     core loop — this hook provisions the few things the image does NOT carry
#     and warms the build cache so the first `cargo test` is instant.
#   * Idempotent and non-interactive: safe to re-run on resume/clear/compact.
#   * NOT `set -e`: every network/provisioning step is best-effort. The web
#     network policy only lets the in-scope repo through, so an out-of-scope
#     clone (the JSONTestSuite submodule) gets a 403 — that must never abort the
#     hook, and the tests that need it skip gracefully on their own.
set -uo pipefail

cd "${CLAUDE_PROJECT_DIR:-/home/user/lark}" || exit 0

# --- 1. Enable the pre-push fast gate (fmt + cargo test --all) ---------------
# The gate lives in .githooks/pre-push; a fresh clone defaults to .git/hooks,
# so point core.hooksPath at it. Idempotent.
git config core.hooksPath .githooks

# --- 2. Best-effort: check out the JSONTestSuite corpus submodule ------------
# Powers the 293-file `test_json_corpus_against_oracle` test. The test skips
# itself when the submodule is absent, so a blocked clone is non-fatal — we just
# note it so the skipped corpus isn't mistaken for coverage.
if [ ! -e lark-rs/tests/corpora/JSONTestSuite/test_parsing ]; then
  if git submodule update --init lark-rs/tests/corpora/JSONTestSuite >/dev/null 2>&1; then
    echo "submodule: JSONTestSuite checked out (293-file corpus test will run)"
  else
    echo "submodule: JSONTestSuite unreachable (network policy) — corpus test will skip"
  fi
fi

# --- 3. Best-effort: Python deps for oracle regeneration (remote only) -------
# Only tools/generate_oracles.py needs these (interegular/regex); it imports the
# repo's own `lark/` via sys.path, so no editable install is required. Oracles
# are committed JSON, so this is optional — relevant only when touching tools/
# or fixtures/oracles/. Remote-gated so a local session's Python env is left
# untouched.
if [ "${CLAUDE_CODE_REMOTE:-}" = "true" ]; then
  if python3 -m pip install --quiet --disable-pip-version-check interegular regex >/dev/null 2>&1; then
    echo "python: oracle-regen deps ready (interegular, regex)"
  else
    echo "python: oracle-regen deps unavailable — only needed to regenerate oracles"
  fi
fi

# --- 4. Warm the build cache and surface the test status --------------------
echo '=== git ==='
git status -sb
git log --oneline -3
echo '=== lark-rs cargo test (failures only; warms the build cache) ==='
out=$(cargo test --manifest-path lark-rs/Cargo.toml 2>&1) || true
echo "$out" | grep -E 'FAILED|panicked|^error|error\[' | head -40
echo "$out" | awk '/^test result:/ {p+=$4; f+=$6} END {printf "TOTAL: %d passed, %d failed across all test targets\n", p, f}'
echo 'Pre-push gate enabled (.githooks). Before pushing run: lark-rs/scripts/check-fast.sh'
