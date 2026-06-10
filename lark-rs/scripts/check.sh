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

# 2. Rust test suite — identical to the CI "cargo test --all" step. (The L0 lexer
#    differential oracle needs the fancy-oracle feature and runs in step 2a.) It
#    needs the JSONTestSuite submodule for full coverage (it skips that corpus
#    gracefully if absent).
note "Rust tests: cargo test --all"
( cd "$LARK_RS_DIR" && cargo test --all ) || fail "cargo test --all failed"

# 2a. Fancy-oracle differential (docs/LOOKAROUND_SCOPE.md): the default build has
#     zero fancy-regex code, so the L0 whole-lexer differential
#     (tests/test_scanner_differential.rs) only runs under the TEST-ONLY
#     fancy-oracle feature, which resurrects the Regex reference backend's fancy
#     side-probes as the independent oracle. Matches the CI step of the same name.
note "Fancy-oracle differential: cargo test -p lark-rs --features fancy-oracle"
( cd "$LARK_RS_DIR" && cargo test -p lark-rs --features fancy-oracle ) \
  || fail "fancy-oracle differential failed — the lowered engine diverged from the fancy reference"

# 2b. Deterministic super-linearity gate (#56) — the scaling regression net only
#     runs with the perf-counters feature, so it is a no-op in `cargo test --all`.
note "Earley scaling gate: cargo test --features perf-counters --test test_earley_scaling"
( cd "$LARK_RS_DIR" && cargo test --features perf-counters --test test_earley_scaling ) \
  || fail "Earley scaling gate failed — a super-linearity regressed (see test_earley_scaling.rs)"

# 2c. CYK scaling gate (#87) — same perf-counters discipline; asserts the
#     O(n³·|grammar|) table fill stays within a cubic envelope (flat per n³).
note "CYK scaling gate: cargo test --features perf-counters --test test_cyk_scaling"
( cd "$LARK_RS_DIR" && cargo test --features perf-counters --test test_cyk_scaling ) \
  || fail "CYK scaling gate failed — a complexity regression in CNF/DP (see test_cyk_scaling.rs)"

# 2d. Lexer linear-scan gate (#104) — same perf-counters discipline; asserts
#     flat-per-byte per-position scan work via the lexer_scan_steps counter, so an
#     un-anchored fancy-regex forward-scan (the O(n²) pathology) is caught
#     deterministically. Matches the CI "Lexer scaling gate" step.
note "Lexer scaling gate: cargo test --features perf-counters --test test_lexer_scaling"
( cd "$LARK_RS_DIR" && cargo test --features perf-counters --test test_lexer_scaling ) \
  || fail "Lexer scaling gate failed — per-position scan work regressed (see test_lexer_scaling.rs)"

# 2e. Dense-DFA build-cost gate (docs/LEXER_DFA_PLAN.md) — asserts the lookaround
#     lowering's determinized dense-DFA build cost stays flat per terminal and per
#     guard width via the dense_build_bytes counter, so a determinization blowup in
#     the L5 bake target is caught. Matches the CI "Lexer DFA build-cost gate" step.
note "Lexer DFA build-cost gate: cargo test --features perf-counters --test test_lexer_dfa_build_scaling"
( cd "$LARK_RS_DIR" && cargo test --features perf-counters --test test_lexer_dfa_build_scaling ) \
  || fail "Lexer DFA build-cost gate failed — determinization blowup in the lookaround lowering (see test_lexer_dfa_build_scaling.rs)"

# 3. Oracle-freshness gate — regenerate from Python Lark and require no diff.
#    (Needs 'pip install lark' and the JSONTestSuite submodule:
#     git submodule update --init lark-rs/tests/corpora/JSONTestSuite)
note "Oracle freshness: regenerate, expect no diff"
command -v python3 >/dev/null 2>&1 || fail "python3 not installed"
(
  cd "$LARK_RS_DIR"
  python3 tools/generate_oracles.py >/dev/null
  python3 tools/extract_lark_compliance.py >/dev/null
  python3 tools/generate_wild_oracles.py >/dev/null
) || fail "oracle generators failed (needs 'pip install lark regex' — regex backs the wild bank's synapse_storm grammar)"

if ! ( cd "$REPO_ROOT" && git diff --quiet -- lark-rs/tests/fixtures/oracles ); then
  printf '\nCommitted oracles are stale — regeneration changed:\n' >&2
  ( cd "$REPO_ROOT" && git --no-pager diff --stat -- lark-rs/tests/fixtures/oracles ) >&2
  fail "oracle-freshness gate failed — commit the regenerated fixtures"
fi

printf '\n\033[1;32m✅ All gates passed.\033[0m\n'
