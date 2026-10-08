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
  fetch() { return 1; }
  set +e
  latest_tag >/dev/null
  rc=$?
  set -e
  [ "$rc" -eq 10 ]

  # Could not ask at all: no redirect, and the API fails too (a 403 body carries no tag_name).
  latest_tag_url() { return 1; }
  fetch() { printf '%s\n' '{"message":"API rate limit exceeded"}'; }
  set +e
  latest_tag >/dev/null
  rc=$?
  set -e
  [ "$rc" -eq 1 ]

  # And the API still answers when the redirect is the thing that broke.
  latest_tag_url() { return 1; }
  fetch() { printf '%s\n' '{"tag_name": "v1.2.3"}'; }
  [ "$(latest_tag)" = "v1.2.3" ]
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

test_checksum_mismatch_is_fatal
test_missing_asset_is_the_only_fallback
test_a_binary_that_cannot_run_falls_back_to_source
test_a_binary_without_a_version_flag_still_installs
test_latest_tag_separates_absent_from_unreachable
test_wrappers_are_valid_and_leave_no_staging_file
test_wrapper_reclaims_a_dead_update_lock
test_local_checkout_refuses_an_unmatched_version
printf 'install.sh tests: ok\n'
