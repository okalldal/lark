#!/usr/bin/env bash
#
# Local CI gate — runs exactly what GitHub Actions runs, so a red CI is caught
# here before pushing instead of after. Mirrors:
#
#   * Rust format  (.github/workflows/lark-rs.yml)  → cargo fmt --check --all
#   * lark-rs test (.github/workflows/lark-rs.yml)  → cargo test --all
#                                                      + oracle-freshness gate
#
# Exits non-zero on the first failing gate. Run manually any time:
#
#   lark-rs/scripts/check.sh
#
# It is also invoked automatically by the committed pre-push hook
# (.githooks/pre-push); enable that once per clone with:
#
#   git config core.hooksPath .githooks
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LARK_RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$LARK_RS_DIR/.." && pwd)"

note() { printf '\n\033[1;34m▶ %s\033[0m\n' "$1"; }
fail() { printf '\n\033[1;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

# 1. Rust format gate — identical to the CI "fmt" job.
note "Rust format: cargo fmt --check --all"
( cd "$LARK_RS_DIR" && cargo fmt --check --all ) || fail "cargo fmt --check failed — run 'cargo fmt --all' in lark-rs/"

# 2. Rust test suite — identical to the CI "cargo test --all" step.
note "Rust tests: cargo test --all"
( cd "$LARK_RS_DIR" && cargo test --all ) || fail "cargo test --all failed"

# 2b. Deterministic super-linearity gate (#56) — the scaling regression net only
#     runs with the perf-counters feature, so it is a no-op in `cargo test --all`.
note "Earley scaling gate: cargo test --features perf-counters --test test_earley_scaling"
( cd "$LARK_RS_DIR" && cargo test --features perf-counters --test test_earley_scaling ) \
  || fail "Earley scaling gate failed — a super-linearity regressed (see test_earley_scaling.rs)"

# 3. Oracle-freshness gate — regenerate from Python Lark and require no diff.
#    (Needs 'pip install lark' and the JSONTestSuite submodule:
#     git submodule update --init lark-rs/tests/corpora/JSONTestSuite)
note "Oracle freshness: regenerate, expect no diff"
command -v python3 >/dev/null 2>&1 || fail "python3 not installed"
(
  cd "$LARK_RS_DIR"
  python3 tools/generate_oracles.py >/dev/null
  python3 tools/extract_lark_compliance.py >/dev/null
) || fail "oracle generators failed (is Python 'lark' installed? 'pip install lark')"

if ! ( cd "$REPO_ROOT" && git diff --quiet -- lark-rs/tests/fixtures/oracles ); then
  printf '\nCommitted oracles are stale — regeneration changed:\n' >&2
  ( cd "$REPO_ROOT" && git --no-pager diff --stat -- lark-rs/tests/fixtures/oracles ) >&2
  fail "oracle-freshness gate failed — commit the regenerated fixtures"
fi

printf '\n\033[1;32m✅ All gates passed.\033[0m\n'
