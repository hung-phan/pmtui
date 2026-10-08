#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

test "$(cat CLAUDE.md)" = "@AGENTS.md"
test -s AGENTS.md
test -f .agents/skills/pmtui-ui-testing/SKILL.md
test -L .claude/skills/pmtui-ui-testing
test "$(readlink .claude/skills/pmtui-ui-testing)" = "../../.agents/skills/pmtui-ui-testing"
cmp -s \
  .agents/skills/pmtui-ui-testing/SKILL.md \
  .claude/skills/pmtui-ui-testing/SKILL.md

if grep -R -nE 'uses: [^#]+@(v[0-9]+|main|master|stable)([[:space:]]|$)' .github/workflows; then
  echo "workflow actions must be pinned to immutable SHAs" >&2
  exit 1
fi
test "$(
  grep -R -F \
    'uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1' \
    .github/workflows | wc -l
)" -eq 6

grep -Fq 'ref: ${{ env.RELEASE_REF }}' .github/workflows/release.yml
grep -Fq 'os: macos-15-intel' .github/workflows/release.yml
test "$(grep -Fc 'os: macos-13' .github/workflows/release.yml)" -eq 0
grep -Fq 'cargo test --test integration -- --ignored --test-threads=1' \
  .github/workflows/ci.yml

# RELEASE VERIFIES THE RELEASE, NOT THE CODE. ci runs the suite on the tagged commit already — in
# parallel, cached, and it is the same suite — so release gates on ci's own fan-in check run instead
# of spending twenty uncached minutes repeating it.
for repeated in \
  'bash tests/coverage_test.sh' \
  'bash tests/install_sh_test.sh' \
  'cargo clippy' \
  'cargo audit' \
  'cargo fmt' \
  'cargo test'; do
  if grep -Fq "$repeated" .github/workflows/release.yml; then
    echo "release must not re-run ci's work ($repeated); gate on ci's check run" >&2
    exit 1
  fi
done
# …and these are the checks only a release can make: the tag names this commit, the version the
# binaries will report, a commit that came through main, ci's verdict on it, and the ARTIFACT running.
for required in \
  'git describe --tags --exact-match HEAD' \
  'cargo metadata --no-deps --format-version 1' \
  'git merge-base --is-ancestor HEAD refs/remotes/origin/main' \
  '/check-runs' \
  'Smoke-test the packaged binaries'; do
  grep -Fq "$required" .github/workflows/release.yml
done

for required in \
  'cargo build --locked' \
  'cargo fmt --all -- --check' \
  'cargo clippy --all-targets -- -D warnings' \
  'cargo audit' \
  'bash tests/coverage_test.sh' \
  'bash tests/install_sh_test.sh' \
  'bash tests/repo_contract_test.sh'; do
  grep -Fq "$required" .github/workflows/ci.yml
done
# Every surface that PROMISES the coverage gate. `release.yml` left this list when it stopped
# re-running ci's suite: the gate belongs where the suite runs, and naming it in two workflows is how
# it came to run twice.
for surface in \
  .github/workflows/ci.yml \
  AGENTS.md \
  README.md; do
  grep -Fq 'bash tests/coverage_test.sh' "$surface"
done
for threshold in \
  '--fail-under-lines 95' \
  '--fail-under-functions 95' \
  '--fail-under-regions 95'; do
  grep -Fq -- "$threshold" tests/coverage_test.sh
done
grep -Fq 'per-file coverage below 95%%' tests/coverage_test.sh
if PM_COVERAGE_REPORT=tests/fixtures/coverage-below.json \
  PM_COVERAGE_ROOT=/repo/src/ \
  PM_COVERAGE_THRESHOLD_ONLY=1 \
  bash tests/coverage_test.sh >/dev/null 2>&1; then
  echo "coverage wrapper must reject a production file below 95%" >&2
  exit 1
fi
if PM_COVERAGE_REPORT=tests/fixtures/coverage-empty.json \
  PM_COVERAGE_ROOT="$PWD/src/" \
  bash tests/coverage_test.sh >/dev/null 2>&1; then
  echo "coverage wrapper must reject a report with missing production files" >&2
  exit 1
fi
# The suite runs exactly once in CI, under coverage: `coverage_test.sh` already runs
# `--all-targets`, so a second bare `cargo test` job would only repeat it.
test "$(grep -Fc 'bash tests/coverage_test.sh' .github/workflows/ci.yml)" -eq 1
if grep -Eq '^[[:space:]]+cargo test$' .github/workflows/ci.yml; then
  echo "the instrumented suite already covers --all-targets; do not run it twice" >&2
  exit 1
fi
grep -Fq 'uses: Swatinem/rust-cache@' .github/workflows/ci.yml
test "$(grep -Fc 'cache-bin: "true"' .github/workflows/ci.yml)" -eq 2
test "$(grep -Fc 'key: cargo-bin-v1' .github/workflows/ci.yml)" -eq 2
test "$(grep -Fc 'shared-key: project-build' .github/workflows/ci.yml)" -eq 2
test "$(grep -Fc 'add-job-id-key: "false"' .github/workflows/ci.yml)" -eq 2
test "$(grep -Fc 'needs: [build_lint, audit]' .github/workflows/ci.yml)" -eq 2
grep -Fq 'cancel-in-progress: true' .github/workflows/ci.yml
grep -Fq 'needs: [build_lint, tests, audit, real_tmux]' \
  .github/workflows/ci.yml
grep -Fq 'if: ${{ always() }}' .github/workflows/ci.yml
for result in \
  BUILD_LINT_RESULT \
  TESTS_RESULT \
  AUDIT_RESULT \
  REAL_TMUX_RESULT; do
  grep -Fq "test \"\$$result\" = success" .github/workflows/ci.yml
done

test ! -e src/worker/trust.rs
if grep -R -n 'ECC_GATEGUARD=off\|ensure_codex_trust' src; then
  echo "runtime must not override host guards or auto-trust projects" >&2
  exit 1
fi

grep -Fq 'pub fn session_name' src/tmux/session_names.rs
if grep -R -n 'fn chat_session_name\|fn loop_session_name' src; then
  echo "split session-name functions must not return" >&2
  exit 1
fi

printf 'repository contract tests: ok\n'
