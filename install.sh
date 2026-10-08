#!/usr/bin/env bash
#
# agent-manager installer — pmd (daemon) + pmtui (dashboard)
#
#   Install / update to the latest release (or build from source if there are
#   no prebuilt binaries for your platform yet):
#
#     curl -fsSL https://raw.githubusercontent.com/hung-phan/pmtui/main/install.sh | bash
#
#   Pass flags after `-s --`, e.g. force a source build:
#
#     curl -fsSL https://raw.githubusercontent.com/hung-phan/pmtui/main/install.sh | bash -s -- --from-source
#
# Subcommands / flags:
#   (default)            install (or reinstall) the latest version
#   --update             update to the latest version (alias: `update`)
#   --uninstall          remove everything this installer put on disk (alias: `uninstall`)
#   --version <tag>      install a specific release tag, e.g. --version v0.1.0
#   --dir <path>         directory for the `pmd`/`pmtui` commands (default: ~/.local/bin)
#   --from-source        always build from source with cargo (never download prebuilt)
#   --no-wrapper         install the real binaries directly (disables auto-update-on-open)
#   --if-newer           (with --update) do nothing unless a newer version is available
#   --quiet              minimal output (used by the background auto-update check)
#   -y, --non-interactive  never prompt; assume "yes"
#   -h, --help           show this help
#
# Auto-update: unless installed with --no-wrapper, the `pmd`/`pmtui` commands are
# thin wrappers that, at most once a day, kick off a background update check and
# then run the real binary immediately (opening is never blocked). Opt out with
# AGENT_MANAGER_NO_UPDATE=1 in your environment.
#
# This whole script is wrapped in main() so `curl | bash` reads it fully before
# running — a half-read pipe can otherwise execute a truncated script.

set -uo pipefail

# ------------------------------------------------------------------ constants
REPO="hung-phan/pmtui"
APP="agent-manager"
BINS="pmd pmtui"

# Overridable install locations (env wins over defaults; --dir wins over env).
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
BIN_DIR="${AGENT_MANAGER_BIN_DIR:-$HOME/.local/bin}"
LIBEXEC_DIR="${AGENT_MANAGER_LIBEXEC:-$HOME/.local/libexec/$APP}"
STATE_DIR="$DATA_HOME/$APP"
CACHE_DIR="$CACHE_HOME/$APP"

# ------------------------------------------------------------------ arg state
ACTION="install"
REQ_VERSION=""      # explicit tag, or empty for "latest"
FROM_SOURCE=0
NO_WRAPPER=0
IF_NEWER=0
QUIET=0
ASSUME_YES=0

# --------------------------------------------------------------------- output
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_RED=$'\033[31m'; C_GRN=$'\033[32m'; C_YEL=$'\033[33m'
  C_BLU=$'\033[34m'; C_DIM=$'\033[2m'; C_BLD=$'\033[1m'; C_RST=$'\033[0m'
else
  C_RED=""; C_GRN=""; C_YEL=""; C_BLU=""; C_DIM=""; C_BLD=""; C_RST=""
fi

info() { [ "$QUIET" -eq 1 ] || printf '%s\n' "${C_BLU}::${C_RST} $*"; }
ok()   { [ "$QUIET" -eq 1 ] || printf '%s\n' "${C_GRN}✓${C_RST} $*"; }
warn() { printf '%s\n' "${C_YEL}!${C_RST} $*" >&2; }
die()  { printf '%s\n' "${C_RED}✗ $*${C_RST}" >&2; exit 1; }

banner() {
  [ "$QUIET" -eq 1 ] && return 0
  printf '%s\n' "${C_BLD}${C_BLU}"
  printf '%s\n' "  ┌───────────────────────────────────────────────┐"
  printf '%s\n' "  │   agent-manager · pmd + pmtui installer         │"
  printf '%s\n' "  └───────────────────────────────────────────────┘"
  printf '%s\n' "${C_RST}"
}

# Read a line from the real terminal even when the script is piped from curl.
prompt_read() { # prompt_read <varname> <prompt>
  local __var="$1" __prompt="$2" __ans=""
  if [ "$ASSUME_YES" -eq 1 ] || [ ! -r /dev/tty ]; then
    printf -v "$__var" '%s' ""
    return 0
  fi
  printf '%s' "$__prompt" >/dev/tty
  IFS= read -r __ans </dev/tty || __ans=""
  printf -v "$__var" '%s' "$__ans"
}

confirm() { # confirm <prompt>  -> 0 if yes
  [ "$ASSUME_YES" -eq 1 ] && return 0
  local reply=""
  prompt_read reply "$1 [y/N] "
  case "$reply" in [yY]|[yY][eE][sS]) return 0 ;; *) return 1 ;; esac
}

have() { command -v "$1" >/dev/null 2>&1; }

# ------------------------------------------------------------------- download
# fetch <url>  -> body on stdout (fails on HTTP errors)
fetch() {
  if have curl; then curl -fsSL "$1"
  elif have wget; then wget -qO- "$1"
  else die "need curl or wget to download"
  fi
}

# download <url> <dest>  -> 0 on success (fails on HTTP errors)
download() {
  if have curl; then curl -fsSL -o "$2" "$1"
  elif have wget; then wget -qO "$2" "$1"
  else die "need curl or wget to download"
  fi
}

sha256_of() { # sha256_of <file> -> bare hex hash
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else die "need sha256sum or shasum to verify downloads"
  fi
}

# ------------------------------------------------------------------- platform
detect_platform() {
  local os arch
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  case "$os" in
    linux)  OS="linux" ;;
    darwin) OS="darwin" ;;
    *) die "unsupported OS: $os (only linux and macOS are supported)" ;;
  esac
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64)  ARCH="amd64" ;;
    arm64|aarch64) ARCH="arm64" ;;
    *) die "unsupported architecture: $arch (only amd64/arm64)" ;;
  esac
  IS_WSL=0
  if grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null || [ -n "${WSL_DISTRO_NAME:-}" ]; then
    IS_WSL=1
  fi
}

# latest_tag -> release tag on stdout, or empty if there are no releases
# Where `releases/latest` redirects to. A repository WITH releases lands on
# `/releases/tag/<tag>`; one with none lands on `/releases`. Plain HTTP, not the REST API, so it is
# not subject to the API's unauthenticated hourly limit.
latest_tag_url() {
  if have curl; then curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest"
  elif have wget; then wget -qO /dev/null --max-redirect=5 "https://github.com/$REPO/releases/latest" 2>&1 \
    | sed -nE 's#^Location: (https://[^ ]*).*#\1#p' | tail -n1
  else die "need curl or wget to download"
  fi
}

# The newest published release tag on stdout.
#
# Exit status SEPARATES THREE OUTCOMES, because conflating them lied to the user: 0 = found,
# 10 = GitHub answered and there are no releases, 1 = GitHub could not be asked at all.
#
# This used to read the REST API alone. That endpoint allows 60 unauthenticated calls an hour PER
# IP, so one shared office or cloud NAT exhausts it for everyone behind it — and the empty reply
# was indistinguishable from "no releases". The script then announced "No published releases
# found" and spent minutes building from source, both of which were false, with a perfectly good
# binary published the whole time. Measured here: the API returned HTTP 403 while the redirect
# resolved the tag fine.
latest_tag() {
  local url tag
  if url="$(latest_tag_url)" && [ -n "$url" ]; then
    case "$url" in
      */releases/tag/*)
        printf '%s\n' "${url##*/releases/tag/}"
        return 0
        ;;
      */releases|*/releases/)
        return 10 # asked, answered: this repository has published nothing
        ;;
    esac
  fi
  # The redirect did not resolve (old wget, a proxy that rewrites it). Try the API, which may well
  # be throttled — but a throttled API is still worth asking before giving up.
  tag="$(fetch "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
    | grep -m1 '"tag_name"' \
    | sed -E 's/.*"tag_name" *: *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || return 1
  printf '%s\n' "$tag"
}

# ------------------------------------------------------------ argument parser
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      install)           ACTION="install" ;;
      update|--update)   ACTION="update" ;;
      uninstall|--uninstall) ACTION="uninstall" ;;
      --version)         shift; REQ_VERSION="${1:-}"; [ -n "$REQ_VERSION" ] || die "--version needs a tag" ;;
      --dir)             shift; BIN_DIR="${1:-}"; [ -n "$BIN_DIR" ] || die "--dir needs a path" ;;
      --from-source)     FROM_SOURCE=1 ;;
      --no-wrapper)      NO_WRAPPER=1 ;;
      --if-newer)        IF_NEWER=1 ;;
      --quiet)           QUIET=1 ;;
      -y|--non-interactive) ASSUME_YES=1 ;;
      -h|--help)         usage; exit 0 ;;
      *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
  done
}

usage() {
  sed -n '3,45p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//' || cat <<'EOF'
agent-manager installer — see the header of install.sh for full usage.
Common: install (default) | --update | --uninstall | --from-source | --help
EOF
}

# ----------------------------------------------------------------- toolchains
ensure_tmux() {
  have tmux && return 0
  warn "tmux is not installed — pmd and pmtui drive agents over tmux and need it at runtime."
  case "$OS" in
    darwin) warn "install it with:  brew install tmux" ;;
    linux)  warn "install it with your package manager, e.g.  sudo apt-get install tmux  /  sudo dnf install tmux" ;;
  esac
}

ensure_build_deps() {
  have git   || die "building from source needs git — install it and re-run"
  have cargo || die "building from source needs the Rust toolchain (cargo) — see https://rustup.rs and re-run"
}

# --------------------------------------------------------------- install core
umask 022
TMPDIR_INSTALL=""
cleanup() { [ -n "$TMPDIR_INSTALL" ] && rm -rf "$TMPDIR_INSTALL" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

# Install a binary by staging it NEXT TO the destination, then renaming — so the
# final step is a true same-filesystem atomic rename(2): it never truncates the live
# path, leaves no ENOENT window, and is ETXTBSY-safe (a running process keeps its old
# inode). The build/extract source may live on another filesystem (e.g. /tmp), where a
# bare `mv` would be a non-atomic copy — hence staging on the destination fs first. A
# failed move aborts (die) instead of being silently recorded as a successful install.
install_binary() { # install_binary <src> <dest>
  local staged="$2.new.$$"
  cp "$1" "$staged" && chmod +x "$staged" && mv -f "$staged" "$2" \
    || { rm -f "$staged" 2>/dev/null; die "failed to install $2"; }
}

# Try prebuilt binaries from GitHub Releases. Echoes the installed tag on success;
# returns 1 (no output) to signal "fall back to source".
install_prebuilt() { # install_prebuilt <tag>
  local tag="$1" ver asset url tar sums
  ver="${tag#v}"
  asset="${APP}_${ver}_${OS}_${ARCH}.tar.gz"
  url="https://github.com/$REPO/releases/download/$tag/$asset"
  TMPDIR_INSTALL="$(mktemp -d)"
  tar="$TMPDIR_INSTALL/$asset"
  sums="$TMPDIR_INSTALL/checksums.txt"

  info "Downloading $asset ($tag)…" >&2
  if ! download "$url" "$tar"; then
    info "No prebuilt binary for ${OS}/${ARCH} at $tag." >&2
    return 10
  fi

  # Fail closed: we must be able to fetch checksums.txt AND match this asset.
  if ! download "https://github.com/$REPO/releases/download/$tag/checksums.txt" "$sums"; then
    die "downloaded $asset but could not fetch checksums.txt — refusing to install an unverified binary"
  fi
  local want got
  want="$(grep -E "[ *]${asset}\$" "$sums" | awk '{print $1}' | head -n1)"
  [ -n "$want" ] || die "$asset is not listed in checksums.txt — refusing to install an unverified binary"
  got="$(sha256_of "$tar")"
  [ "$want" = "$got" ] || die "checksum mismatch for $asset (expected $want, got $got) — refusing to install a tampered/corrupt binary"
  ok "Verified SHA-256 for $asset" >&2

  tar -xzf "$tar" -C "$TMPDIR_INSTALL" || die "failed to extract $asset"

  # VERIFY, DO NOT PREDICT — and verify BEFORE installing anything.
  #
  # A tarball built for the right OS and architecture can still be unable to run here. A binary
  # linked against a newer glibc than this machine provides dies with `version GLIBC_x.yz not
  # found`, and every check above passes: the download succeeded, the checksum matched, the
  # architecture is right. The install then reports success and leaves the user with commands that
  # cannot start — which is worse than building from source, because it looks like it worked.
  #
  # Predicting it would mean parsing `ldd --version` and comparing it against a floor this script
  # cannot see, for every libc a host might have. Asking the binary to run answers the only
  # question that matters, in one line, and catches a wrong architecture, a missing shared library
  # and a truncated archive with the same check.
  #
  # Both binaries are proved in the temp directory before either is installed, so a release that
  # cannot run on this machine never reaches $LIBEXEC_DIR and never replaces a working install.
  # `--help`, not `--version`: `pmd` accepts both but `pmtui --version` exits 2, so probing with
  # it would fail on every machine and send every install to a source build. Both binaries answer
  # `--help` with status 0, and a binary that cannot load its interpreter never gets that far.
  local b path why
  for b in $BINS; do
    path="$(prebuilt_binary "$b" "$asset")"
    chmod +x "$path" 2>/dev/null || true
    if ! "$path" --help >/dev/null 2>&1; then
      why="$("$path" --help 2>&1 | head -n1)"
      warn "the prebuilt $b for ${OS}/${ARCH} cannot run on this machine"
      [ -n "$why" ] && info "  $why" >&2
      return 10
    fi
  done
  ok "Prebuilt binaries run on this machine" >&2

  mkdir -p "$LIBEXEC_DIR"
  for b in $BINS; do
    install_binary "$(prebuilt_binary "$b" "$asset")" "$LIBEXEC_DIR/$b"
  done
  printf '%s\n' "$tag"
}

# Locate one binary inside the extracted release archive. Prefers an executable match, because an
# archive can legitimately carry a same-named non-executable file beside it.
prebuilt_binary() { # prebuilt_binary <binary> <asset-name-for-errors>
  local b="$1" asset="$2" path
  path="$(find "$TMPDIR_INSTALL" -type f -name "$b" -perm -u+x 2>/dev/null | head -n1)"
  [ -z "$path" ] && path="$(find "$TMPDIR_INSTALL" -type f -name "$b" 2>/dev/null | head -n1)"
  [ -n "$path" ] || die "$b was not found inside $asset"
  printf '%s\n' "$path"
}

# Build from a git checkout in the state dir (clones once, then pulls). If this
# script is being run from inside a checkout, build that instead of cloning.
build_from_source() { # build_from_source [tag]  -> echoes the installed version marker
  ensure_build_deps
  local tag="${1:-}" src here
  here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" 2>/dev/null && pwd || true)"

  if [ -n "$here" ] && [ -f "$here/Cargo.toml" ] && grep -q "$APP" "$here/Cargo.toml" 2>/dev/null; then
    src="$here"
    if [ -n "$tag" ]; then
      local head wanted
      head="$(git -C "$src" rev-parse HEAD 2>/dev/null || true)"
      wanted="$(git -C "$src" rev-parse "${tag}^{commit}" 2>/dev/null || true)"
      [ -n "$wanted" ] || die "could not resolve requested version $tag in the local checkout"
      [ "$head" = "$wanted" ] \
        || die "requested $tag does not match the local checkout HEAD; run the installer outside the checkout"
    fi
    info "Building from the local checkout at $src…" >&2
  else
    src="$STATE_DIR/source"
    if [ -d "$src/.git" ]; then
      info "Updating source checkout at $src…" >&2
      git -C "$src" fetch --quiet --tags origin || warn "git fetch failed; building the checkout as-is"
    else
      info "Cloning $REPO into $src…" >&2
      mkdir -p "$STATE_DIR"
      git clone --quiet "https://github.com/$REPO.git" "$src" || die "git clone failed"
    fi
    if [ -n "$tag" ]; then
      git -C "$src" checkout --quiet "$tag" || die "could not check out $tag"
    else
      git -C "$src" checkout --quiet origin/HEAD 2>/dev/null \
        || git -C "$src" pull --quiet --ff-only 2>/dev/null || true
    fi
  fi

  info "Compiling (cargo build --release --locked)… this can take a few minutes." >&2
  ( cd "$src" && cargo build --release --locked ) || die "cargo build failed"
  mkdir -p "$LIBEXEC_DIR"
  local b
  for b in $BINS; do
    [ -f "$src/target/release/$b" ] || die "$b was not produced by the build"
    install_binary "$src/target/release/$b" "$LIBEXEC_DIR/$b"
  done

  local sha
  sha="$(git -C "$src" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  printf '%s\n' "source@$sha"
}

# Write the launcher wrappers (bin/pmd, bin/pmtui) that self-update on open.
write_wrappers() {
  mkdir -p "$BIN_DIR"
  local b w
  for b in $BINS; do
    w="$BIN_DIR/$b"
    local staged="$w.new.$$"
    cat > "$staged" <<EOF
#!/usr/bin/env bash
# agent-manager launcher for '$b' — throttled, non-blocking auto-update on open.
# Opt out entirely with: AGENT_MANAGER_NO_UPDATE=1
SELF="$b"
LIBEXEC="$LIBEXEC_DIR"
STATE="$STATE_DIR"
CACHE="$CACHE_DIR"
REAL="\$LIBEXEC/\$SELF"

if [ -z "\${AGENT_MANAGER_NO_UPDATE:-}" ] && [ -f "\$STATE/install.sh" ]; then
  stamp="\$CACHE/last-update-check"
  now="\$(date +%s 2>/dev/null || echo 0)"
  last=0; [ -f "\$stamp" ] && last="\$(cat "\$stamp" 2>/dev/null || echo 0)"
  if [ "\$now" -ge "\$((last + 86400))" ] 2>/dev/null; then
    mkdir -p "\$CACHE" 2>/dev/null || true
    lock="\$CACHE/update.lock"
    if [ -d "\$lock" ]; then
      owner="\$(cat "\$lock/pid" 2>/dev/null || true)"
      lock_mtime="\$(stat -c %Y "\$lock" 2>/dev/null \\
        || stat -f %m "\$lock" 2>/dev/null || echo "\$now")"
      if { [ -n "\$owner" ] && ! kill -0 "\$owner" 2>/dev/null; } \\
          || [ "\$now" -ge "\$((lock_mtime + 86400))" ] 2>/dev/null; then
        stale="\$lock.stale.\$\$"
        if mv "\$lock" "\$stale" 2>/dev/null; then
          rm -rf "\$stale"
        fi
      fi
    fi
    if mkdir "\$lock" 2>/dev/null; then
      # Non-blocking and single-flight. Record success only after the update
      # completes, so a network/build failure is retried on the next launch.
      (
        owner="\${BASHPID:-\$\$}"
        printf '%s\n' "\$owner" > "\$lock/pid" 2>/dev/null || true
        cleanup_update_lock() {
          if [ "\$(cat "\$lock/pid" 2>/dev/null || true)" = "\$owner" ]; then
            rm -f "\$lock/pid" 2>/dev/null || true
            rmdir "\$lock" 2>/dev/null || true
          fi
        }
        trap cleanup_update_lock EXIT
        if bash "\$STATE/install.sh" --update --if-newer --quiet --non-interactive \\
            >>"\$STATE/update.log" 2>&1 </dev/null; then
          printf '%s\n' "\$now" > "\$stamp.new.\$\$" 2>/dev/null \\
            && mv -f "\$stamp.new.\$\$" "\$stamp" 2>/dev/null
        fi
      ) >/dev/null 2>&1 &
    fi
  fi
fi

exec "\$REAL" "\$@"
EOF
    chmod +x "$staged" && mv -f "$staged" "$w" \
      || { rm -f "$staged" 2>/dev/null; die "failed to install wrapper $w"; }
  done
}

# Record what we installed so --uninstall can remove exactly that.
write_manifest() { # write_manifest <version-marker>
  mkdir -p "$STATE_DIR"
  printf '%s\n' "$1" > "$STATE_DIR/version"
  {
    local b
    for b in $BINS; do
      printf '%s\n' "$LIBEXEC_DIR/$b"
      printf '%s\n' "$BIN_DIR/$b"
    done
  } > "$STATE_DIR/install-manifest"
  # Keep a copy of ourselves so the wrappers (and `--update`) work after the
  # original `curl | bash` pipe is gone.
  if [ -f "${BASH_SOURCE[0]:-}" ] && [ "${BASH_SOURCE[0]:-}" != "$STATE_DIR/install.sh" ]; then
    cp -f "${BASH_SOURCE[0]}" "$STATE_DIR/install.sh" 2>/dev/null || true
  fi
  if [ ! -f "$STATE_DIR/install.sh" ]; then
    # Running via `curl | bash` (no file on disk): fetch a copy for the wrappers.
    download "https://raw.githubusercontent.com/$REPO/main/install.sh" "$STATE_DIR/install.sh" 2>/dev/null || true
  fi
  chmod +x "$STATE_DIR/install.sh" 2>/dev/null || true
}

path_advice() {
  case ":$PATH:" in
    *":$BIN_DIR:"*) return 0 ;;
  esac
  warn "$BIN_DIR is not on your PATH."
  local rc="your shell profile"
  case "${SHELL:-}" in
    */zsh)  rc="~/.zshrc" ;;
    */bash) rc="~/.bashrc" ;;
  esac
  printf '%s\n' "  Add this line to ${C_BLD}$rc${C_RST} and restart your shell:" >&2
  printf '%s\n' "    ${C_BLD}export PATH=\"$BIN_DIR:\$PATH\"${C_RST}" >&2
}

verify_install() {
  local pmd="$BIN_DIR/pmd"
  [ -x "$pmd" ] || pmd="$LIBEXEC_DIR/pmd"
  if [ -x "$pmd" ] && "$pmd" --help >/dev/null 2>&1; then
    ok "pmd and pmtui are installed."
  else
    warn "installed the files but 'pmd --help' did not run cleanly — check $LIBEXEC_DIR"
  fi
}

do_install() {
  banner
  detect_platform
  ensure_tmux

  local marker="" tag=""
  if [ "$FROM_SOURCE" -eq 1 ]; then
    marker="$(build_from_source "$REQ_VERSION")"
  else
    tag="$REQ_VERSION"
    if [ -z "$tag" ]; then
      # Name the real reason for a source build. "No published releases found" was printed for a
      # rate-limited API too, which sent users to a multi-minute compile — or to a dead end if they
      # had no Rust toolchain — while a working binary sat published.
      set +e
      tag="$(latest_tag)"
      local latest_rc=$?
      set -e
      case "$latest_rc" in
        0) ;;
        10) info "No published releases found." ;;
        *) warn "could not reach GitHub to find the latest release (rate limit, proxy or network)" ;;
      esac
    fi
    if [ -n "$tag" ]; then
      if marker="$(install_prebuilt "$tag")"; then
        :
      else
        local prebuilt_rc=$?
        [ "$prebuilt_rc" -eq 10 ] || exit "$prebuilt_rc"
        marker=""
      fi
    fi
    # No `else` here: the reason there is no tag was already named above, by outcome. A second
    # blanket "No published releases found" is how the wrong cause got printed in the first place.
    if [ -z "$marker" ]; then
      info "Falling back to building from source."
      marker="$(build_from_source "$REQ_VERSION")"
    fi
  fi

  # A failed install_binary die()s inside the command substitution above, so an empty
  # marker means nothing landed — never record that as a successful install.
  [ -n "$marker" ] || die "installation failed — no binaries were installed"

  if [ "$NO_WRAPPER" -eq 1 ]; then
    mkdir -p "$BIN_DIR"
    local b
    for b in $BINS; do
      install_binary "$LIBEXEC_DIR/$b" "$BIN_DIR/$b"
    done
  else
    write_wrappers
  fi
  write_manifest "$marker"

  ok "Installed ${C_BLD}$marker${C_RST} → $BIN_DIR/{pmd,pmtui}"
  [ "$NO_WRAPPER" -eq 1 ] || info "Auto-update-on-open is on (opt out with AGENT_MANAGER_NO_UPDATE=1)."
  path_advice
  verify_install
  [ "$QUIET" -eq 1 ] || cat <<EOF

${C_BLD}Get started:${C_RST}
  pmtui                                          # open the dashboard
  pmd --registry ./registry.json --socket pmd    # run the daemon
  pmtui --help / pmd --help                      # options

Uninstall any time:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | bash -s -- --uninstall
EOF
}

do_update() {
  detect_platform
  local cur=""
  [ -f "$STATE_DIR/version" ] && cur="$(cat "$STATE_DIR/version" 2>/dev/null || echo "")"

  if [ "$IF_NEWER" -eq 1 ]; then
    case "$cur" in
      source@*)
        # Source install: "newer" means the tracked branch moved.
        local src="$STATE_DIR/source"
        if [ -d "$src/.git" ]; then
          git -C "$src" fetch --quiet origin 2>/dev/null || true
          local head up
          head="$(git -C "$src" rev-parse HEAD 2>/dev/null || echo x)"
          up="$(git -C "$src" rev-parse '@{u}' 2>/dev/null || git -C "$src" rev-parse origin/HEAD 2>/dev/null || echo y)"
          [ "$head" = "$up" ] && { info "Already up to date."; return 0; }
        fi
        FROM_SOURCE=1
        ;;
      *)
        local remote=""
        remote="$(latest_tag)"
        [ -n "$remote" ] && [ "$remote" = "$cur" ] && { info "Already up to date ($cur)."; return 0; }
        ;;
    esac
  fi

  info "Updating (current: ${cur:-none})…"
  do_install
}

do_uninstall() {
  banner
  info "This removes the pmd/pmtui commands and agent-manager's own files."
  info "Your registry (~/.config/pmd) and each project's on-disk state are left untouched."
  if ! confirm "Uninstall agent-manager?"; then
    info "Cancelled."; return 0
  fi
  if [ -f "$STATE_DIR/install-manifest" ]; then
    local f
    while IFS= read -r f; do
      [ -n "$f" ] && rm -f "$f" 2>/dev/null && info "removed $f"
    done < "$STATE_DIR/install-manifest"
  else
    # No manifest — remove the well-known paths.
    local b
    for b in $BINS; do rm -f "$BIN_DIR/$b" "$LIBEXEC_DIR/$b" 2>/dev/null; done
  fi
  # STATE_DIR/CACHE_DIR always end in /$APP, so they are safe to remove outright.
  # LIBEXEC_DIR may be an env override pointing at a shared dir (AGENT_MANAGER_LIBEXEC
  # is used verbatim), so only remove it if our binaries left it empty — never rm -rf
  # a directory we don't own.
  rm -rf "$STATE_DIR" "$CACHE_DIR" 2>/dev/null || true
  rmdir "$LIBEXEC_DIR" 2>/dev/null || true
  ok "agent-manager uninstalled."
}

main() {
  parse_args "$@"
  case "$ACTION" in
    install)   do_install ;;
    update)    do_update ;;
    uninstall) do_uninstall ;;
    *) die "internal: unknown action $ACTION" ;;
  esac
}

# Allow tests/review to `source` this file without executing it.
if [ -z "${AGENT_MANAGER_INSTALL_SH_SOURCE_ONLY:-}" ]; then
  main "$@"
fi
