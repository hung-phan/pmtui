#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -n "${PM_COVERAGE_REPORT:-}" ]]; then
  REPORT="$PM_COVERAGE_REPORT"
else
  REPORT="$(mktemp "${TMPDIR:-/tmp}/agent-manager-coverage.XXXXXX.json")"
  trap 'rm -f "$REPORT"' EXIT

  # CLEAN FIRST. Stale `.profraw` from an earlier run is counted by the aggregating `report` below, so
  # a gap introduced since then can be papered over by coverage that no longer exists — which is
  # exactly how this file's own gate passed locally while CI (always a fresh runner) failed on
  # `render/sending.rs`. A gate that can lie is not a gate.
  cargo llvm-cov clean --workspace
  cargo llvm-cov --all-targets --no-report -- --test-threads=1
  cargo llvm-cov --no-clean --test integration --json --output-path "$REPORT" -- \
    enter_routing --ignored --test-threads=1
  cargo llvm-cov --no-clean --test integration --json --output-path "$REPORT" -- \
    confirmed_takeover_replaces_only_the_dashboard --ignored --test-threads=1
  cargo llvm-cov report --json --output-path "$REPORT" \
    --fail-under-lines 95 \
    --fail-under-functions 95 \
    --fail-under-regions 95
fi

failures="$(
  jq -r --arg root "${PM_COVERAGE_ROOT:-$ROOT/src/}" '
    .data[0].files[]
    | select(.filename | startswith($root))
    | select((.filename | contains("/tests/") | not)
        and (.filename | endswith("/tests.rs") | not)
        and (.filename | endswith("/test_support.rs") | not))
    | select(.summary.lines.percent < 95
        or .summary.functions.percent < 95
        or .summary.regions.percent < 95)
    | "\(.filename | ltrimstr($root)): regions=\(.summary.regions.percent)% functions=\(.summary.functions.percent)% lines=\(.summary.lines.percent)%"
  ' "$REPORT"
)"

if [[ "${PM_COVERAGE_THRESHOLD_ONLY:-0}" != 1 ]]; then
  coverage_root="${PM_COVERAGE_ROOT:-$ROOT/src/}"
  expected_files="$(
    find "${coverage_root%/}" -type f -name '*.rs' \
      ! -path '*/tests/*' \
      ! -name 'tests.rs' \
      ! -name 'test_support.rs' \
      ! -path '*/src/advise/mod.rs' \
      ! -path '*/src/bin/pmtui/mode.rs' \
      ! -path '*/src/escalation/mod.rs' \
      ! -path '*/src/lib.rs' \
      ! -path '*/src/spawn/mod.rs' \
      ! -path '*/src/state/mod.rs' \
      ! -path '*/src/tmux/mod.rs' \
      ! -path '*/src/worker/mod.rs' \
      ! -path '*/src/worker/result.rs' \
      -print | sort
  )"
  reported_files="$(
    jq -r --arg root "$coverage_root" '
      .data[0].files[]
      | select(.filename | startswith($root))
      | select((.filename | contains("/tests/") | not)
          and (.filename | endswith("/tests.rs") | not)
          and (.filename | endswith("/test_support.rs") | not))
      | .filename
    ' "$REPORT" | sort -u
  )"
  missing_files="$(comm -23 <(printf '%s\n' "$expected_files") <(printf '%s\n' "$reported_files"))"
  if [[ -z "$expected_files" || -n "$missing_files" ]]; then
    printf 'production files missing from coverage report:\n%s\n' "$missing_files" >&2
    exit 1
  fi
fi

if [[ -n "$failures" ]]; then
  printf 'per-file coverage below 95%%:\n%s\n' "$failures" >&2
  exit 1
fi

printf 'coverage tests: every production file is at least 95%% for regions, functions, and lines\n'
