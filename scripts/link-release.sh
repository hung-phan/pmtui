#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${PM_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}"
BUILD=1

usage() {
  cat <<'EOF'
Usage: scripts/link-release.sh [--no-build] [--dir PATH]

Build pmd and pmtui in release mode, then symlink them into PATH.

  --no-build   Link existing release binaries without compiling
  --dir PATH   Link directory (default: PM_INSTALL_DIR or ~/.local/bin)
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --no-build)
      BUILD=0
      ;;
    --dir)
      [ "$#" -ge 2 ] || { echo "--dir requires a path" >&2; exit 2; }
      BIN_DIR="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$BIN_DIR" in
  /*) ;;
  *) BIN_DIR="$PWD/$BIN_DIR" ;;
esac
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in
  /*) ;;
  *) TARGET_DIR="$ROOT/$TARGET_DIR" ;;
esac
RELEASE_DIR="${PM_RELEASE_DIR:-$TARGET_DIR/release}"
case "$RELEASE_DIR" in
  /*) ;;
  *) RELEASE_DIR="$PWD/$RELEASE_DIR" ;;
esac

if [ "$BUILD" -eq 1 ]; then
  (
    cd "$ROOT"
    CARGO_TARGET_DIR="$TARGET_DIR" \
      "${CARGO:-cargo}" build --release --locked --bin pmd --bin pmtui
  )
fi

mkdir -p "$BIN_DIR"
for name in pmd pmtui; do
  target="$RELEASE_DIR/$name"
  link="$BIN_DIR/$name"
  { [ -f "$target" ] && [ -x "$target" ]; } || {
    echo "release binary is missing or not executable: $target" >&2
    exit 1
  }
  if [ -e "$link" ] && [ ! -L "$link" ]; then
    echo "refusing to replace non-symlink: $link" >&2
    exit 1
  fi
done

for name in pmd pmtui; do
  target="$RELEASE_DIR/$name"
  link="$BIN_DIR/$name"
  ln -sfn "$target" "$link"
  printf 'linked %s -> %s\n' "$link" "$target"
done

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) printf 'add %s to PATH to use pmd and pmtui\n' "$BIN_DIR" ;;
esac
