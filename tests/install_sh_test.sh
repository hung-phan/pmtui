#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export AGENT_MANAGER_INSTALL_SH_SOURCE_ONLY=1
# shellcheck source=../install.sh
source "$ROOT/install.sh"

test_checksum_mismatch_is_fatal() (
  OS=linux
  ARCH=amd64
  QUIET=1
  download() {
    case "$2" in
      *checksums.txt)
        printf '%s\n' \
          "deadbeef  agent-manager_1.0.0_linux_amd64.tar.gz" >"$2"
        ;;
      *) printf 'payload' >"$2" ;;
    esac
  }
  sha256_of() { printf 'cafebabe\n'; }

  set +e
  marker="$(install_prebuilt v1.0.0)"
  rc=$?
  set -e
  [ "$rc" -ne 0 ]
  [ "$rc" -ne 10 ]
  [ -z "$marker" ]
)

test_missing_asset_is_the_only_fallback() (
  OS=linux
  ARCH=amd64
  QUIET=1
  download() { return 1; }

  set +e
  install_prebuilt v1.0.0 >/dev/null
  rc=$?
  set -e
  [ "$rc" -eq 10 ]
)

# A fake release tarball whose two binaries behave however a test needs them to, so the run-probe
# in `install_prebuilt` can be driven without a real cross-platform binary.
stage_fake_release() { # stage_fake_release <dir> <pmd-body> <pmtui-body> -> tarball path
  local dir="$1" pmd_body="$2" pmtui_body="$3" name=agent-manager_1.0.0_linux_amd64
  mkdir -p "$dir/$name"
  printf '#!/bin/sh\n%s\n' "$pmd_body" >"$dir/$name/pmd"
  printf '#!/bin/sh\n%s\n' "$pmtui_body" >"$dir/$name/pmtui"
  chmod +x "$dir/$name/pmd" "$dir/$name/pmtui"
  tar -C "$dir" -czf "$dir/$name.tar.gz" "$name"
  printf '%s\n' "$dir/$name.tar.gz"
}

# THE FAILURE v0.1.0 SHIPPED. The download succeeds, the checksum matches and the architecture is
# right — and the binary still cannot start, because it was linked against a newer glibc than the
# host has. Every check that existed passed, the install reported success, and the user was left
# with commands that die on launch. The probe must catch it from the binary's own behaviour, and
# must catch it BEFORE anything is installed.
test_a_binary_that_cannot_run_falls_back_to_source() (
  OS=linux
  ARCH=amd64
  QUIET=1
  local work src rc
  work="$(mktemp -d)"
  src="$(stage_fake_release "$work" \
    'echo "pmd: /lib64/libc.so.6: version GLIBC_2.39 not found" >&2; exit 1' \
    'exit 0')"
  LIBEXEC_DIR="$work/libexec"
  download() {
    case "$2" in
      *checksums.txt) printf 'aa  agent-manager_1.0.0_linux_amd64.tar.gz\n' >"$2" ;;
      *) cp "$src" "$2" ;;
    esac
  }
  sha256_of() { printf 'aa\n'; }

  set +e
  install_prebuilt v1.0.0 >/dev/null 2>&1
  rc=$?
  set -e
  [ "$rc" -eq 10 ]
  # NOTHING was installed: an unusable release must never replace a working one.
  [ ! -e "$LIBEXEC_DIR/pmd" ]
  [ ! -e "$LIBEXEC_DIR/pmtui" ]
)

# THE PROBE'S OWN FOOTGUN. `pmd` answers both `--help` and `--version`, but `pmtui --version` exits
# 2 — so probing with `--version` would reject every healthy release on every machine and send all
# installs to a source build. This pins the real binaries' contract so the probe cannot regress to
# a flag only one of them accepts.
test_a_binary_without_a_version_flag_still_installs() (
  OS=linux
  ARCH=amd64
  QUIET=1
  local work src marker
  work="$(mktemp -d)"
  src="$(stage_fake_release "$work" \
    'exit 0' \
    'case "$1" in --help) exit 0 ;; *) echo "pmtui: unknown argument" >&2; exit 2 ;; esac')"
  LIBEXEC_DIR="$work/libexec"
  download() {
    case "$2" in
      *checksums.txt) printf 'aa  agent-manager_1.0.0_linux_amd64.tar.gz\n' >"$2" ;;
      *) cp "$src" "$2" ;;
    esac
  }
  sha256_of() { printf 'aa\n'; }

  marker="$(install_prebuilt v1.0.0)"
  [ "$marker" = "v1.0.0" ]
  [ -x "$LIBEXEC_DIR/pmd" ]
  [ -x "$LIBEXEC_DIR/pmtui" ]
)

# A RATE LIMIT IS NOT AN ABSENCE. The REST API allows 60 unauthenticated calls an hour per IP, so a
# shared office or cloud NAT exhausts it for everyone behind it. Reading only that endpoint made a
# 403 indistinguishable from "this repo has no releases": the installer announced the latter and
# spent minutes compiling, with a working binary published the whole time. Measured — the API
# returned 403 from this host while the redirect resolved the tag.
test_latest_tag_separates_absent_from_unreachable() (
  QUIET=1
  local rc

  # Found: the redirect lands on /releases/tag/<tag>.
  latest_tag_url() { printf 'https://github.com/o/r/releases/tag/v9.9.9\n'; }
  [ "$(latest_tag)" = "v9.9.9" ]

  # Genuinely none: the redirect lands on the releases index instead.
  latest_tag_url() { printf 'https://github.com/o/r/releases\n'; }
  latest_tag_api() { return 1; }
  set +e
  latest_tag >/dev/null
  rc=$?
  set -e
  [ "$rc" -eq 10 ]

  # Could not ask at all: no redirect, and the API fails too (a 403 body carries no tag_name).
  latest_tag_url() { return 1; }
  latest_tag_api() { printf '%s\n' '{"message":"API rate limit exceeded"}'; }
  set +e
  latest_tag >/dev/null
  rc=$?
  set -e
  [ "$rc" -eq 1 ]

  # And the API still answers when the redirect is the thing that broke.
  latest_tag_url() { return 1; }
  latest_tag_api() { printf '%s\n' '{"tag_name": "v1.2.3"}'; }
  [ "$(latest_tag)" = "v1.2.3" ]
)

# BOTH network calls in `latest_tag` must be time-capped, because the dashboard's Settings row runs
# `--check` inline on its event loop: an unbounded wait there is a pmtui that has stopped repainting
# with nothing on screen to say why. `fetch` is deliberately NOT capped — it also downloads
# tarballs, and `curl -m` limits TOTAL time, which would abort a slow but healthy download.
test_the_release_lookup_cannot_wait_forever() (
  local body
  for fn in latest_tag_url latest_tag_api; do
    body="$(declare -f "$fn")"
    case "$body" in
      *"-m 10"*) ;;
      *) printf 'FAIL: %s does not cap its curl request\n' "$fn" >&2; return 1 ;;
    esac
    case "$body" in
      *"--timeout=10"*) ;;
      *) printf 'FAIL: %s does not cap its wget request\n' "$fn" >&2; return 1 ;;
    esac
  done
  case "$(declare -f fetch)" in
    *-m*) printf 'FAIL: fetch caps total time, which would abort a slow download\n' >&2; return 1 ;;
  esac
)

# THE BUG THIS PINS. `write_manifest` copies `${BASH_SOURCE[0]}` over the stored script, which is a
# no-op when the stored script is what is running — so an update launched by the wrappers (or by the
# dashboard's Updates row) left the copy frozen at whatever version first installed the machine.
# Measured on a real machine: binaries at v0.1.5, stored script too old to understand `--check`,
# and `--update --if-newer` answering "Already up to date" without touching it. Forever.
test_an_update_refreshes_the_stored_copy_of_the_installer() (
  local dir staged
  dir="$(mktemp -d)"
  trap 'rm -rf "$dir"' EXIT
  STATE_DIR="$dir/state"
  mkdir -p "$STATE_DIR"
  printf '%s\n' "an old installer that cannot answer --check" > "$STATE_DIR/install.sh"

  # Stand in for the network: `download` is the one seam `refresh_self` reaches it through.
  download() { printf '%s\n' 'install_binary() { :; }' 'latest_tag() { :; }' > "$2"; }
  refresh_self v9.9.9
  grep -q '^install_binary()' "$STATE_DIR/install.sh" || {
    printf 'FAIL: the stored installer was not refreshed\n' >&2; return 1; }
  [ -x "$STATE_DIR/install.sh" ] || { printf 'FAIL: not executable\n' >&2; return 1; }

  # A FAILED download must leave the working script alone — a machine with no installer at all is
  # worse than one with an old installer.
  printf '%s\n' 'install_binary() { :; }' 'latest_tag() { :; }' 'marker=keep' > "$STATE_DIR/install.sh"
  download() { return 1; }
  refresh_self v9.9.9
  grep -q 'marker=keep' "$STATE_DIR/install.sh" || {
    printf 'FAIL: a failed download destroyed the installer\n' >&2; return 1; }

  # So must a download that is NOT this installer: a proxy error page is a 200 with a body.
  download() { printf '%s\n' '<html>404</html>' > "$2"; }
  refresh_self v9.9.9
  grep -q 'marker=keep' "$STATE_DIR/install.sh" || {
    printf 'FAIL: an error page replaced the installer\n' >&2; return 1; }

  # And no staging file is left behind on any of those paths.
  staged="$(find "$STATE_DIR" -name 'install.sh.new.*' | head -n1)"
  [ -z "$staged" ] || { printf 'FAIL: left a staging file: %s\n' "$staged" >&2; return 1; }
)

# `--if-newer` RETURNS before `do_install`, so a refresh placed after it would never run on the one
# path that matters: current binaries, stale script.
test_the_self_refresh_runs_before_the_up_to_date_shortcut() (
  local body
  body="$(declare -f do_update)"
  case "$body" in
    *refresh_self*) ;;
    *) printf 'FAIL: do_update never refreshes the stored installer\n' >&2; return 1 ;;
  esac
  # Textual ORDER, because the shortcut is a `return`: whatever comes after it is unreachable.
  case "${body%%IF_NEWER*}" in
    *refresh_self*) ;;
    *) printf 'FAIL: the refresh is behind the --if-newer shortcut\n' >&2; return 1 ;;
  esac
)

test_wrappers_are_valid_and_leave_no_staging_file() (
  local dir
  dir="$(mktemp -d)"
  trap 'rm -rf "$dir"' EXIT
  BIN_DIR="$dir/bin"
  LIBEXEC_DIR="$dir/libexec"
  STATE_DIR="$dir/state"
  CACHE_DIR="$dir/cache"
  mkdir -p "$BIN_DIR" "$LIBEXEC_DIR" "$STATE_DIR"
  write_wrappers
  bash -n "$BIN_DIR/pmd" "$BIN_DIR/pmtui"
  [ ! -e "$BIN_DIR/pmd.new.$$" ]
  [ ! -e "$BIN_DIR/pmtui.new.$$" ]
)

test_wrapper_reclaims_a_dead_update_lock() (
  local dir
  dir="$(mktemp -d)"
  trap 'rm -rf "$dir"' EXIT
  BIN_DIR="$dir/bin"
  LIBEXEC_DIR="$dir/libexec"
  STATE_DIR="$dir/state"
  CACHE_DIR="$dir/cache"
  mkdir -p "$BIN_DIR" "$LIBEXEC_DIR" "$STATE_DIR" "$CACHE_DIR/update.lock"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$LIBEXEC_DIR/pmd"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$LIBEXEC_DIR/pmtui"
  printf '#!/usr/bin/env bash\nexit 0\n' >"$STATE_DIR/install.sh"
  chmod +x "$LIBEXEC_DIR/pmd" "$LIBEXEC_DIR/pmtui" "$STATE_DIR/install.sh"
  printf '99999999\n' >"$CACHE_DIR/update.lock/pid"
  write_wrappers

  "$BIN_DIR/pmd"
  for _ in $(seq 1 100); do
    [ -f "$CACHE_DIR/last-update-check" ] && [ ! -e "$CACHE_DIR/update.lock" ] && break
    sleep 0.02
  done
  [ -f "$CACHE_DIR/last-update-check" ]
  [ ! -e "$CACHE_DIR/update.lock" ]
)

test_local_checkout_refuses_an_unmatched_version() (
  QUIET=1
  set +e
  marker="$(build_from_source v0.0.0-does-not-exist 2>/dev/null)"
  rc=$?
  set -e
  [ "$rc" -ne 0 ]
  [ -z "$marker" ]
)

# `--check` answers the dashboard's Settings row, so its THREE outcomes have to stay apart.
# `latest_tag` already separates them by exit code (0 found / 10 nothing published / 1 could not
# ask), and flattening that into "no update" would tell the human they are current when the truth
# is that GitHub was unreachable.
test_check_reports_current_and_latest() (
  QUIET=1
  STATE_DIR="$(mktemp -d)"
  printf 'v0.1.2\n' >"$STATE_DIR/version"
  latest_tag() { printf 'v0.1.6\n'; }

  out="$(do_check)"
  [ "$?" -eq 0 ]
  printf '%s\n' "$out" | grep -qx 'current=v0.1.2'
  printf '%s\n' "$out" | grep -qx 'latest=v0.1.6'
  rm -rf "$STATE_DIR"
)

test_check_reports_an_unknown_current_version() (
  QUIET=1
  STATE_DIR="$(mktemp -d)"   # no version marker: never installed by the installer
  latest_tag() { printf 'v0.1.6\n'; }

  out="$(do_check)"
  [ "$?" -eq 0 ]
  printf '%s\n' "$out" | grep -qx 'current='
  printf '%s\n' "$out" | grep -qx 'latest=v0.1.6'
  rm -rf "$STATE_DIR"
)

test_check_distinguishes_unreachable_from_no_releases() (
  QUIET=1
  STATE_DIR="$(mktemp -d)"
  printf 'v0.1.2\n' >"$STATE_DIR/version"

  # GitHub answered: nothing published.
  latest_tag() { return 10; }
  set +e
  out="$(do_check)"
  rc=$?
  set -e
  [ "$rc" -eq 10 ]
  printf '%s\n' "$out" | grep -qx 'latest='

  # GitHub could not be asked at all — NOT the same answer.
  latest_tag() { return 1; }
  set +e
  out="$(do_check)"
  rc=$?
  set -e
  [ "$rc" -eq 1 ]
  printf '%s\n' "$out" | grep -qx 'latest='
  rm -rf "$STATE_DIR"
)

test_checksum_mismatch_is_fatal
test_missing_asset_is_the_only_fallback
test_check_reports_current_and_latest
test_check_reports_an_unknown_current_version
test_check_distinguishes_unreachable_from_no_releases
test_a_binary_that_cannot_run_falls_back_to_source
test_a_binary_without_a_version_flag_still_installs
test_latest_tag_separates_absent_from_unreachable
test_the_release_lookup_cannot_wait_forever
test_an_update_refreshes_the_stored_copy_of_the_installer
test_the_self_refresh_runs_before_the_up_to_date_shortcut
test_wrappers_are_valid_and_leave_no_staging_file
test_wrapper_reclaims_a_dead_update_lock
test_local_checkout_refuses_an_unmatched_version
printf 'install.sh tests: ok\n'
