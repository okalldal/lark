#!/usr/bin/env bash
#
# Fast local gate — the Pareto cut of the full CI (lark-rs/scripts/check.sh).
# Runs the two checks that catch nearly every red CI:
#
#   * cargo fmt --check --all   (the most common trivial red)
#   * cargo test --all          (compile errors + the whole functional suite)
#
# Deliberately skipped (the PR's GitHub Actions run covers them — fix via the
# CI callback, don't pre-run them locally):
#
#   * fancy-oracle differential   (a second feature build for one test target)
#   * perf-counters scaling gates (a third feature build)
#   * python.lark LALR build gate (slow, #[ignore]d)
#   * oracle-freshness regen      (needs Python + pip installs; only relevant
#                                  when you touched tools/ or fixtures/oracles/
#                                  — if you did, run check.sh's step 3 yourself)
#   * python/wasm binding jobs    (separate crates; only relevant when you
#                                  touched lark-rs/python/ or lark-rs/wasm/)
#
# This is the pre-push hook's gate (.githooks/pre-push). For reproducing a red
# CI locally in full, use lark-rs/scripts/check.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LARK_RS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

note() { printf '\n\033[1;34m▶ %s\033[0m\n' "$1"; }
fail() { printf '\n\033[1;31m❌ %s\033[0m\n' "$1" >&2; exit 1; }

note "Rust format: cargo fmt --check --all"
( cd "$LARK_RS_DIR" && cargo fmt --check --all ) || fail "cargo fmt --check failed — run 'cargo fmt --all' in lark-rs/"

note "Rust tests: cargo test --all"
( cd "$LARK_RS_DIR" && cargo test --all ) || fail "cargo test --all failed"

printf '\n\033[1;32m✅ Fast gate passed — push, open the PR, and let CI run the full matrix.\033[0m\n'
